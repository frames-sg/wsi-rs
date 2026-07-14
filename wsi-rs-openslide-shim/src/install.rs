use std::ffi::CStr;
use std::fs;
use std::io::{Read, Write};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformLibraryNames {
    MacOS,
    Linux,
}

impl PlatformLibraryNames {
    pub fn current() -> Result<Self, String> {
        if cfg!(target_os = "macos") {
            Ok(Self::MacOS)
        } else if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else {
            Err("wsi_rs OpenSlide shim install supports macOS and Linux only".into())
        }
    }

    pub fn names(self) -> [&'static str; 3] {
        match self {
            Self::MacOS => [
                "libopenslide.1.dylib",
                "libopenslide.dylib",
                "libopenslide.4.dylib",
            ],
            Self::Linux => ["libopenslide.so.1", "libopenslide.so", "libopenslide.so.4"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStep {
    Backup { from: PathBuf, to: PathBuf },
    CopyShim { from: PathBuf, to: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreEntry {
    pub destination: PathBuf,
    pub backup: Option<PathBuf>,
}

pub fn install_destinations(prefix: &Path, platform: PlatformLibraryNames) -> Vec<PathBuf> {
    platform
        .names()
        .into_iter()
        .map(|name| prefix.join("lib").join(name))
        .collect()
}

pub fn plan_install(
    prefix: &Path,
    shim: &Path,
    platform: PlatformLibraryNames,
    stamp: u64,
    exists: impl Fn(&Path) -> bool,
) -> Vec<InstallStep> {
    let mut steps = Vec::new();
    for destination in install_destinations(prefix, platform) {
        if exists(&destination) {
            steps.push(InstallStep::Backup {
                from: destination.clone(),
                to: backup_path(&destination, stamp),
            });
        }
        steps.push(InstallStep::CopyShim {
            from: shim.to_path_buf(),
            to: destination,
        });
    }
    steps
}

pub fn execute_install(
    prefix: &Path,
    shim: &Path,
    platform: PlatformLibraryNames,
    stamp: u64,
) -> Result<PathBuf, String> {
    if !shim.is_file() {
        return Err(format!("shim library does not exist: {}", shim.display()));
    }
    let lib_dir = prefix.join("lib");
    fs::create_dir_all(&lib_dir).map_err(|err| format!("create {}: {err}", lib_dir.display()))?;
    let lib_dir = lib_dir
        .canonicalize()
        .map_err(|err| format!("resolve {}: {err}", lib_dir.display()))?;
    reject_symlink(shim, "shim library")?;

    let destinations = platform
        .names()
        .into_iter()
        .map(|name| lib_dir.join(name))
        .collect::<Vec<_>>();
    let entries = destinations
        .into_iter()
        .map(|destination| RestoreEntry {
            backup: destination
                .exists()
                .then(|| backup_path(&destination, stamp)),
            destination,
        })
        .collect::<Vec<_>>();
    let stages = entries
        .iter()
        .map(|entry| stage_path(&entry.destination, stamp))
        .collect::<Vec<_>>();
    let manifest = lib_dir.join(".wsi-rs-openslide-shim-install.tsv");
    if manifest.exists() {
        return Err(format!(
            "an installation manifest already exists; restore it first: {}",
            manifest.display()
        ));
    }
    preflight_install(&entries, &stages)?;

    for stage in &stages {
        if let Err(err) = copy_and_sync(shim, stage) {
            cleanup_paths(&stages);
            return Err(err);
        }
    }
    if let Err(err) = write_manifest(&manifest, &entries, "prepared") {
        cleanup_paths(&stages);
        return Err(err);
    }

    let commit_result = (|| {
        for (entry, stage) in entries.iter().zip(&stages) {
            if let Some(backup) = &entry.backup {
                fs::rename(&entry.destination, backup).map_err(|err| {
                    format!(
                        "backup {} to {}: {err}",
                        entry.destination.display(),
                        backup.display()
                    )
                })?;
            }
            fs::rename(stage, &entry.destination).map_err(|err| {
                format!(
                    "commit staged shim {} to {}: {err}",
                    stage.display(),
                    entry.destination.display()
                )
            })?;
        }

        let verify_target = entries
            .first()
            .map(|entry| entry.destination.as_path())
            .ok_or_else(|| "no install destinations planned".to_string())?;
        verify_library_version(verify_target)?;
        write_manifest(&manifest, &entries, "installed")?;
        sync_directory(&lib_dir)
    })();

    if let Err(err) = commit_result {
        return match rollback_install(&entries, &stages, &manifest) {
            Ok(()) => Err(format!("install failed and was rolled back: {err}")),
            Err(rollback_err) => Err(format!(
                "install failed: {err}; rollback also failed: {rollback_err}; recovery manifest: {}",
                manifest.display()
            )),
        };
    }

    Ok(manifest)
}

pub fn execute_restore(prefix: &Path, stamp: u64) -> Result<(), String> {
    let manifest = manifest_path(prefix);
    let (state, entries) = read_and_validate_manifest(prefix, &manifest)?;
    let mut removed_destinations = Vec::new();
    let mut restored_backups = Vec::new();
    for entry in &entries {
        if let Some(backup) = &entry.backup {
            if !backup.exists() {
                if state == "installed" {
                    rollback_restore(&restored_backups, &removed_destinations);
                    return Err(format!("backup is missing: {}", backup.display()));
                }
                // A prepared manifest with no backup means this destination
                // was never committed, so its original file stays in place.
                continue;
            }
        }
        if entry.destination.exists() {
            let removed = removed_path(&entry.destination, stamp);
            if removed.exists() {
                rollback_restore(&restored_backups, &removed_destinations);
                return Err(format!(
                    "restore side path already exists: {}",
                    removed.display()
                ));
            }
            if let Err(err) = fs::rename(&entry.destination, &removed) {
                rollback_restore(&restored_backups, &removed_destinations);
                return Err(format!(
                    "move installed shim {} to {}: {err}",
                    entry.destination.display(),
                    removed.display()
                ));
            }
            removed_destinations.push((entry.destination.clone(), removed));
        }
        if let Some(backup) = &entry.backup {
            if let Err(err) = fs::rename(backup, &entry.destination) {
                rollback_restore(&restored_backups, &removed_destinations);
                return Err(format!(
                    "restore {} to {}: {err}",
                    backup.display(),
                    entry.destination.display()
                ));
            }
            restored_backups.push((entry.destination.clone(), backup.clone()));
        }
    }
    for (_, removed) in removed_destinations {
        if removed.exists() {
            fs::remove_file(&removed)
                .map_err(|err| format!("remove restored shim {}: {err}", removed.display()))?;
        }
    }
    fs::remove_file(&manifest)
        .map_err(|err| format!("remove restore manifest {}: {err}", manifest.display()))?;
    Ok(())
}

pub fn manifest_path(prefix: &Path) -> PathBuf {
    prefix
        .join("lib")
        .join(".wsi-rs-openslide-shim-install.tsv")
}

fn backup_path(destination: &Path, stamp: u64) -> PathBuf {
    PathBuf::from(format!("{}.wsi_rs-backup-{stamp}", destination.display()))
}

fn removed_path(destination: &Path, stamp: u64) -> PathBuf {
    PathBuf::from(format!("{}.wsi_rs-removed-{stamp}", destination.display()))
}

fn stage_path(destination: &Path, stamp: u64) -> PathBuf {
    PathBuf::from(format!("{}.wsi_rs-stage-{stamp}", destination.display()))
}

fn write_manifest(path: &Path, entries: &[RestoreEntry], state: &str) -> Result<(), String> {
    let temporary = path.with_extension("tsv.tmp");
    let mut file = fs::File::options()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|err| format!("create {}: {err}", temporary.display()))?;
    writeln!(file, "wsi-rs-openslide-shim\t1\t{state}")
        .map_err(|err| format!("write {}: {err}", temporary.display()))?;
    for entry in entries {
        let backup = entry
            .backup
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        writeln!(file, "{}\t{}", entry.destination.display(), backup)
            .map_err(|err| format!("write {}: {err}", temporary.display()))?;
    }
    file.sync_all()
        .map_err(|err| format!("sync {}: {err}", temporary.display()))?;
    fs::rename(&temporary, path)
        .map_err(|err| format!("commit manifest {}: {err}", path.display()))?;
    sync_directory(
        path.parent()
            .ok_or_else(|| "manifest has no parent".to_string())?,
    )?;
    Ok(())
}

fn read_manifest(path: &Path) -> Result<(String, Vec<RestoreEntry>), String> {
    let mut text = String::new();
    fs::File::open(path)
        .map_err(|err| format!("open {}: {err}", path.display()))?
        .read_to_string(&mut text)
        .map_err(|err| format!("read {}: {err}", path.display()))?;
    let mut lines = text.lines();
    let header = lines
        .next()
        .ok_or_else(|| "manifest is empty".to_string())?;
    let Some(("wsi-rs-openslide-shim", rest)) = header.split_once('\t') else {
        return Err("manifest header is invalid".into());
    };
    let Some(("1", state @ ("prepared" | "installed"))) = rest.split_once('\t') else {
        return Err("manifest version or state is invalid".into());
    };
    let mut entries = Vec::new();
    for (idx, line) in lines.enumerate() {
        let Some((destination, backup)) = line.split_once('\t') else {
            return Err(format!("manifest line {} is malformed", idx + 2));
        };
        entries.push(RestoreEntry {
            destination: PathBuf::from(destination),
            backup: (!backup.is_empty()).then(|| PathBuf::from(backup)),
        });
    }
    Ok((state.to_string(), entries))
}

fn preflight_install(entries: &[RestoreEntry], stages: &[PathBuf]) -> Result<(), String> {
    for (entry, stage) in entries.iter().zip(stages) {
        if entry.destination.exists() {
            reject_symlink(&entry.destination, "install destination")?;
        }
        if entry.backup.as_ref().is_some_and(|backup| backup.exists()) {
            return Err(format!(
                "backup path already exists: {}",
                entry.backup.as_ref().unwrap().display()
            ));
        }
        if stage.exists() {
            return Err(format!("stage path already exists: {}", stage.display()));
        }
    }
    Ok(())
}

fn copy_and_sync(source: &Path, destination: &Path) -> Result<(), String> {
    fs::copy(source, destination).map_err(|err| {
        format!(
            "stage {} to {}: {err}",
            source.display(),
            destination.display()
        )
    })?;
    fs::File::open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|err| format!("sync staged shim {}: {err}", destination.display()))
}

fn rollback_install(
    entries: &[RestoreEntry],
    stages: &[PathBuf],
    manifest: &Path,
) -> Result<(), String> {
    let mut errors = Vec::new();
    for (entry, stage) in entries.iter().zip(stages).rev() {
        if let Some(backup) = entry.backup.as_ref().filter(|backup| backup.exists()) {
            if entry.destination.exists() {
                if let Err(err) = fs::remove_file(&entry.destination) {
                    errors.push(format!("remove {}: {err}", entry.destination.display()));
                    continue;
                }
            }
            if let Err(err) = fs::rename(backup, &entry.destination) {
                errors.push(format!(
                    "restore {} to {}: {err}",
                    backup.display(),
                    entry.destination.display()
                ));
            }
        } else if entry.backup.is_none() && entry.destination.exists() {
            if let Err(err) = fs::remove_file(&entry.destination) {
                errors.push(format!("remove {}: {err}", entry.destination.display()));
            }
        }
        if stage.exists() {
            if let Err(err) = fs::remove_file(stage) {
                errors.push(format!("remove {}: {err}", stage.display()));
            }
        }
    }
    if errors.is_empty() && manifest.exists() {
        fs::remove_file(manifest).map_err(|err| format!("remove {}: {err}", manifest.display()))?;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn cleanup_paths(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn read_and_validate_manifest(
    prefix: &Path,
    manifest: &Path,
) -> Result<(String, Vec<RestoreEntry>), String> {
    reject_symlink(manifest, "restore manifest")?;
    let lib_dir = prefix
        .join("lib")
        .canonicalize()
        .map_err(|err| format!("resolve prefix library directory: {err}"))?;
    let (state, entries) = read_manifest(manifest)?;
    if entries.is_empty() {
        return Err("restore manifest has no entries".into());
    }
    let allowed = PlatformLibraryNames::MacOS
        .names()
        .into_iter()
        .chain(PlatformLibraryNames::Linux.names())
        .collect::<std::collections::HashSet<_>>();
    let mut destinations = std::collections::HashSet::new();
    for entry in &entries {
        if entry.destination.parent() != Some(lib_dir.as_path())
            || !entry
                .destination
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| allowed.contains(name))
        {
            return Err(format!(
                "restore destination is outside the supported prefix: {}",
                entry.destination.display()
            ));
        }
        if !destinations.insert(entry.destination.clone()) {
            return Err(format!(
                "duplicate restore destination: {}",
                entry.destination.display()
            ));
        }
        if let Some(backup) = &entry.backup {
            let destination_name = entry.destination.file_name().unwrap().to_string_lossy();
            let expected_prefix = format!("{destination_name}.wsi_rs-backup-");
            let valid_name = backup
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix(&expected_prefix))
                .is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit())
                });
            if backup.parent() != Some(lib_dir.as_path()) || !valid_name {
                return Err(format!("invalid restore backup path: {}", backup.display()));
            }
        }
    }
    Ok((state, entries))
}

fn reject_symlink(path: &Path, description: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|err| format!("inspect {description} {}: {err}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{description} must not be a symlink: {}",
            path.display()
        ));
    }
    Ok(())
}

fn rollback_restore(restored: &[(PathBuf, PathBuf)], moved: &[(PathBuf, PathBuf)]) {
    for (destination, backup) in restored.iter().rev() {
        if destination.exists() && !backup.exists() {
            let _ = fs::rename(destination, backup);
        }
    }
    for (destination, removed) in moved.iter().rev() {
        if !destination.exists() && removed.exists() {
            let _ = fs::rename(removed, destination);
        }
    }
}

fn sync_directory(path: &Path) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|err| format!("sync directory {}: {err}", path.display()))
}

fn verify_library_version(path: &Path) -> Result<(), String> {
    // SAFETY: The loaded library is immediately queried for the documented
    // OpenSlide version symbol, and the returned pointer is checked for NULL
    // before conversion to a C string.
    unsafe {
        let library = libloading::Library::new(path)
            .map_err(|err| format!("load {}: {err}", path.display()))?;
        let get_version: libloading::Symbol<unsafe extern "C" fn() -> *const c_char> = library
            .get(b"openslide_get_version\0")
            .map_err(|err| format!("load openslide_get_version from {}: {err}", path.display()))?;
        let version = get_version();
        if version.is_null() {
            return Err("openslide_get_version returned NULL".into());
        }
        let version = CStr::from_ptr(version).to_string_lossy();
        if !version.starts_with("OpenSlide-wsi-rs") {
            return Err(format!("unexpected OpenSlide shim version: {version}"));
        }
    }
    Ok(())
}
