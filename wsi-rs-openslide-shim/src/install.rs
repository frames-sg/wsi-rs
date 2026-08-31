use std::ffi::CStr;
use std::fs;
use std::io::{Read, Write};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};

const MAX_INSTALL_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_INSTALL_MANIFEST_ENTRIES: usize = 3;
const LEGACY_LIBRARY_NAMES: [&str; 2] = ["libopenslide.4.dylib", "libopenslide.so.4"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformLibraryNames {
    MacOS,
    Linux,
    Windows,
}

impl PlatformLibraryNames {
    pub fn current() -> Result<Self, String> {
        if cfg!(target_os = "macos") {
            Ok(Self::MacOS)
        } else if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else if cfg!(target_os = "windows") {
            Ok(Self::Windows)
        } else {
            Err("wsi_rs OpenSlide shim install supports macOS, Linux, and Windows only".into())
        }
    }

    pub fn names(self) -> &'static [&'static str] {
        match self {
            Self::MacOS => &["libopenslide.1.dylib", "libopenslide.dylib"],
            Self::Linux => &["libopenslide.so.1", "libopenslide.so"],
            Self::Windows => &["libopenslide-1.dll"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreEntry {
    pub destination: PathBuf,
    pub backup: Option<PathBuf>,
}

/// Typed failure returned by the shim installer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InstallError {
    Operation {
        message: String,
    },
    RolledBack {
        primary: String,
    },
    RollbackFailed {
        primary: String,
        rollback: String,
        preserved_backups: Vec<PathBuf>,
        recovery_manifest: PathBuf,
    },
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operation { message } => formatter.write_str(message),
            Self::RolledBack { primary } => {
                write!(formatter, "install failed and was rolled back: {primary}")
            }
            Self::RollbackFailed {
                primary,
                rollback,
                preserved_backups,
                recovery_manifest,
            } => {
                write!(
                    formatter,
                    "install failed: {primary}; rollback also failed: {rollback}; recovery manifest: {}",
                    recovery_manifest.display()
                )?;
                if !preserved_backups.is_empty() {
                    write!(formatter, "; preserved backups:")?;
                    for backup in preserved_backups {
                        write!(formatter, " {}", backup.display())?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for InstallError {}

impl From<String> for InstallError {
    fn from(message: String) -> Self {
        Self::Operation { message }
    }
}

pub fn install_destinations(prefix: &Path, platform: PlatformLibraryNames) -> Vec<PathBuf> {
    platform
        .names()
        .iter()
        .map(|name| prefix.join("lib").join(name))
        .collect()
}

pub fn execute_install(
    prefix: &Path,
    shim: &Path,
    platform: PlatformLibraryNames,
    stamp: u64,
) -> Result<PathBuf, String> {
    execute_install_detailed(prefix, shim, platform, stamp).map_err(|error| error.to_string())
}

/// Execute an install while preserving typed recovery information when both
/// the primary operation and rollback fail.
pub fn execute_install_detailed(
    prefix: &Path,
    shim: &Path,
    platform: PlatformLibraryNames,
    stamp: u64,
) -> Result<PathBuf, InstallError> {
    if !shim.is_file() {
        return Err(format!("shim library does not exist: {}", shim.display()).into());
    }
    let lib_dir = prefix.join("lib");
    fs::create_dir_all(&lib_dir).map_err(|err| format!("create {}: {err}", lib_dir.display()))?;
    let lib_dir = lib_dir
        .canonicalize()
        .map_err(|err| format!("resolve {}: {err}", lib_dir.display()))?;
    reject_symlink(shim, "shim library")?;

    let destinations = platform
        .names()
        .iter()
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
    if path_entry_exists(&manifest)? {
        return Err(format!(
            "an installation manifest already exists; restore it first: {}",
            manifest.display()
        )
        .into());
    }
    let temporary_manifest = manifest.with_extension("tsv.tmp");
    if path_entry_exists(&temporary_manifest)? {
        return Err(format!(
            "manifest temporary path already exists: {}",
            temporary_manifest.display()
        )
        .into());
    }
    preflight_install(&entries, &stages)?;

    for stage in &stages {
        if let Err(err) = copy_and_sync(shim, stage) {
            cleanup_paths(&stages);
            return Err(err.into());
        }
    }
    if let Err(err) = write_manifest(&manifest, &entries, "prepared") {
        cleanup_paths(&stages);
        return Err(err.into());
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

        // Every supported platform declares exactly three loader-compatible
        // names, so the install plan is nonempty by construction.
        let verify_target = entries[0].destination.as_path();
        verify_library_version(verify_target)?;
        write_manifest(&manifest, &entries, "installed")?;
        sync_directory(&lib_dir)
    })();

    if let Err(err) = commit_result {
        return match rollback_install(&entries, &stages, &manifest) {
            Ok(()) => Err(InstallError::RolledBack { primary: err }),
            Err(rollback_err) => Err(combined_install_error(
                err,
                rollback_err,
                &entries,
                &manifest,
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
    let result = (|| {
        let mut file = fs::File::options()
            .create_new(true)
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
        drop(file);
        fs::rename(&temporary, path)
            .map_err(|err| format!("commit manifest {}: {err}", path.display()))?;
        let Some(parent) = path.parent() else {
            return Err("manifest has no parent".to_string());
        };
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_manifest(path: &Path) -> Result<(String, Vec<RestoreEntry>), String> {
    let file = fs::File::open(path).map_err(|err| format!("open {}: {err}", path.display()))?;
    let mut bytes = Vec::with_capacity(4 * 1024);
    file.take(MAX_INSTALL_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("read {}: {err}", path.display()))?;
    if bytes.len() as u64 > MAX_INSTALL_MANIFEST_BYTES {
        return Err(format!(
            "restore manifest exceeds {MAX_INSTALL_MANIFEST_BYTES} byte safety limit"
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|err| format!("read {} as UTF-8: {err}", path.display()))?;
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
        if entries.len() == MAX_INSTALL_MANIFEST_ENTRIES {
            return Err(format!(
                "manifest has more than {MAX_INSTALL_MANIFEST_ENTRIES} entries"
            ));
        }
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
        if path_entry_exists(&entry.destination)? {
            reject_symlink(&entry.destination, "install destination")?;
        }
        if let Some(backup) = &entry.backup {
            if path_entry_exists(backup)? {
                return Err(format!("backup path already exists: {}", backup.display()));
            }
        }
        if path_entry_exists(stage)? {
            return Err(format!("stage path already exists: {}", stage.display()));
        }
    }
    Ok(())
}

fn copy_and_sync(source: &Path, destination: &Path) -> Result<(), String> {
    let mut source_file =
        fs::File::open(source).map_err(|err| format!("open shim {}: {err}", source.display()))?;
    let mut destination_file = fs::File::options()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|err| format!("create staged shim {}: {err}", destination.display()))?;
    std::io::copy(&mut source_file, &mut destination_file).map_err(|err| {
        format!(
            "stage {} to {}: {err}",
            source.display(),
            destination.display()
        )
    })?;
    destination_file
        .sync_all()
        .map_err(|err| format!("sync staged shim {}: {err}", destination.display()))
}

fn path_entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

fn rollback_install(
    entries: &[RestoreEntry],
    stages: &[PathBuf],
    manifest: &Path,
) -> Result<(), String> {
    rollback_install_with(
        entries,
        stages,
        manifest,
        |path| fs::remove_file(path),
        |from, to| fs::rename(from, to),
    )
}

fn rollback_install_with(
    entries: &[RestoreEntry],
    stages: &[PathBuf],
    manifest: &Path,
    mut remove_file: impl FnMut(&Path) -> std::io::Result<()>,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    for (entry, stage) in entries.iter().zip(stages).rev() {
        if let Some(backup) = entry.backup.as_ref().filter(|backup| backup.exists()) {
            if entry.destination.exists() {
                if let Err(err) = remove_file(&entry.destination) {
                    errors.push(format!("remove {}: {err}", entry.destination.display()));
                    continue;
                }
            }
            if let Err(err) = rename(backup, &entry.destination) {
                errors.push(format!(
                    "restore {} to {}: {err}",
                    backup.display(),
                    entry.destination.display()
                ));
            }
        } else if entry.backup.is_none() && entry.destination.exists() {
            if let Err(err) = remove_file(&entry.destination) {
                errors.push(format!("remove {}: {err}", entry.destination.display()));
            }
        }
        if stage.exists() {
            if let Err(err) = remove_file(stage) {
                errors.push(format!("remove {}: {err}", stage.display()));
            }
        }
    }
    if errors.is_empty() && manifest.exists() {
        remove_file(manifest).map_err(|err| format!("remove {}: {err}", manifest.display()))?;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn combined_install_error(
    primary: String,
    rollback: String,
    entries: &[RestoreEntry],
    manifest: &Path,
) -> InstallError {
    let preserved_backups = entries
        .iter()
        .filter_map(|entry| entry.backup.as_ref())
        .filter(|backup| backup.exists())
        .cloned()
        .collect();
    InstallError::RollbackFailed {
        primary,
        rollback,
        preserved_backups,
        recovery_manifest: manifest.to_path_buf(),
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
        .iter()
        .chain(PlatformLibraryNames::Linux.names().iter())
        .chain(PlatformLibraryNames::Windows.names().iter())
        .copied()
        .chain(LEGACY_LIBRARY_NAMES)
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
        if path_entry_exists(&entry.destination)? {
            reject_symlink(&entry.destination, "restore destination")?;
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
            if path_entry_exists(backup)? {
                reject_symlink(backup, "restore backup")?;
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
        if !version.starts_with("OpenSlide 4.0.1+wsi-rs-") {
            return Err(format!("unexpected OpenSlide shim version: {version}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
