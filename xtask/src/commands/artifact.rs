use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

const OFFICIAL_EXPORTS: [&str; 23] = [
    "openslide_cache_create",
    "openslide_cache_release",
    "openslide_close",
    "openslide_detect_vendor",
    "openslide_get_associated_image_dimensions",
    "openslide_get_associated_image_icc_profile_size",
    "openslide_get_associated_image_names",
    "openslide_get_best_level_for_downsample",
    "openslide_get_error",
    "openslide_get_icc_profile_size",
    "openslide_get_level0_dimensions",
    "openslide_get_level_count",
    "openslide_get_level_dimensions",
    "openslide_get_level_downsample",
    "openslide_get_property_names",
    "openslide_get_property_value",
    "openslide_get_version",
    "openslide_open",
    "openslide_read_associated_image",
    "openslide_read_associated_image_icc_profile",
    "openslide_read_icc_profile",
    "openslide_read_region",
    "openslide_set_cache",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactMetadata {
    schema_version: u32,
    artifact: String,
    target: String,
    signed: bool,
    gpu_feature: String,
    gpu_runtime_required: bool,
    sha256: String,
}

#[derive(Clone, Copy)]
enum ArtifactPlatform {
    MacOS,
    Linux,
    Windows,
}

pub(super) fn smoke(arguments: Vec<String>) -> Result<(), String> {
    if arguments.len() != 4 {
        return Err(
            "artifact-smoke requires <library> <OpenSlide-4.0.1-include-dir> <architecture> <metadata.json>"
                .into(),
        );
    }
    smoke_paths(
        Path::new(&arguments[0]),
        Path::new(&arguments[1]),
        &arguments[2],
        Path::new(&arguments[3]),
    )
}

/// Run the artifact gate during RC preflight when the platform workflow has
/// configured its inputs.  Source-only local preflight remains usable; the
/// release workflow always configures these variables for every artifact cell.
pub(super) fn smoke_configured() -> Result<(), String> {
    let values = [
        env::var("WSI_RS_OPENSLIDE_ARTIFACT").ok(),
        env::var("WSI_RS_OPENSLIDE_INCLUDE_DIR").ok(),
        env::var("WSI_RS_ARTIFACT_ARCH").ok(),
        env::var("WSI_RS_ARTIFACT_METADATA").ok(),
    ];
    if values.iter().all(Option::is_none) {
        eprintln!("artifact smoke is delegated to the required four-platform RC artifact matrix");
        return Ok(());
    }
    if values.iter().any(Option::is_none) {
        return Err("artifact smoke configuration is incomplete; set WSI_RS_OPENSLIDE_ARTIFACT, WSI_RS_OPENSLIDE_INCLUDE_DIR, WSI_RS_ARTIFACT_ARCH, and WSI_RS_ARTIFACT_METADATA".into());
    }
    smoke_paths(
        Path::new(values[0].as_deref().unwrap_or_default()),
        Path::new(values[1].as_deref().unwrap_or_default()),
        values[2].as_deref().unwrap_or_default(),
        Path::new(values[3].as_deref().unwrap_or_default()),
    )
}

fn smoke_paths(
    library: &Path,
    include_dir: &Path,
    expected_architecture: &str,
    metadata_path: &Path,
) -> Result<(), String> {
    if !library.is_file() {
        return Err(format!(
            "OpenSlide artifact is missing: {}",
            library.display()
        ));
    }
    for header in ["openslide.h", "openslide-features.h"] {
        let path = include_dir.join(header);
        if !path.is_file() {
            return Err(format!(
                "official OpenSlide 4.0.1 header is missing: {}",
                path.display()
            ));
        }
    }
    let platform = platform_for_library(library)?;
    validate_metadata(library, metadata_path)?;
    match platform {
        ArtifactPlatform::MacOS => validate_macos(library, expected_architecture)?,
        ArtifactPlatform::Linux => validate_linux(library, expected_architecture)?,
        ArtifactPlatform::Windows => validate_windows(library, expected_architecture)?,
    }
    compile_and_run_smoke(library, include_dir, platform)
}

fn platform_for_library(library: &Path) -> Result<ArtifactPlatform, String> {
    match library.file_name().and_then(OsStr::to_str) {
        Some("libopenslide.1.dylib") => Ok(ArtifactPlatform::MacOS),
        Some("libopenslide.so.1") => Ok(ArtifactPlatform::Linux),
        Some("libopenslide-1.dll") => Ok(ArtifactPlatform::Windows),
        Some(name) => Err(format!(
            "noncanonical OpenSlide 4.x artifact name `{name}`; expected libopenslide.1.dylib, libopenslide.so.1, or libopenslide-1.dll"
        )),
        None => Err("artifact path has no file name".into()),
    }
}

fn validate_metadata(library: &Path, metadata_path: &Path) -> Result<(), String> {
    let bytes = fs::read(metadata_path)
        .map_err(|err| format!("read artifact metadata {}: {err}", metadata_path.display()))?;
    let metadata: ArtifactMetadata = serde_json::from_slice(&bytes)
        .map_err(|err| format!("parse artifact metadata {}: {err}", metadata_path.display()))?;
    let artifact_name = library
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    if metadata.schema_version != 1 || metadata.artifact != artifact_name {
        return Err("artifact metadata schema or artifact name does not match the library".into());
    }
    if metadata.signed {
        return Err(
            "0.7 artifacts must be explicitly unsigned until signing credentials exist".into(),
        );
    }
    if metadata.target.trim().is_empty() || metadata.gpu_feature.trim().is_empty() {
        return Err("artifact metadata target and gpu_feature must be nonempty".into());
    }
    if metadata.sha256.len() != 64
        || !metadata
            .sha256
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err("artifact metadata SHA-256 is malformed".into());
    }
    let actual_sha256 = wsi_rs_perf::sha256_file(library)?;
    if !actual_sha256.eq_ignore_ascii_case(&metadata.sha256) {
        return Err(format!(
            "artifact SHA-256 mismatch for {}: metadata {}, actual {actual_sha256}",
            library.display(),
            metadata.sha256,
        ));
    }
    if metadata.gpu_runtime_required {
        return Err(
            "release artifacts must dynamically fall back when the GPU runtime is absent".into(),
        );
    }
    Ok(())
}

fn validate_linux(library: &Path, expected_architecture: &str) -> Result<(), String> {
    let header = capture("readelf", [OsStr::new("-h"), library.as_os_str()])?;
    let dynamic = capture("readelf", [OsStr::new("-d"), library.as_os_str()])?;
    let exports = capture(
        "nm",
        [
            OsStr::new("-D"),
            OsStr::new("--defined-only"),
            OsStr::new("--extern-only"),
            library.as_os_str(),
        ],
    )?;
    validate_linux_output(&header, &dynamic, &exports, expected_architecture)
}

fn validate_linux_output(
    header: &str,
    dynamic: &str,
    exports: &str,
    expected_architecture: &str,
) -> Result<(), String> {
    require_architecture(header, expected_architecture, "X86-64", "AArch64")?;
    if !dynamic.contains("Library soname: [libopenslide.so.1]") {
        return Err("ELF artifact SONAME must be libopenslide.so.1".into());
    }
    for line in dynamic.lines() {
        if line.contains("Shared library:") {
            let dependency = bracket_value(line).unwrap_or_default();
            if dependency.is_empty() || dependency.contains(['/', '\\']) {
                return Err(format!("invalid ELF dependency entry `{dependency}`"));
            }
        }
        if line.contains("(RPATH)") || line.contains("(RUNPATH)") {
            let value = bracket_value(line).unwrap_or_default();
            for entry in value.split(':') {
                if entry != "$ORIGIN" && !entry.starts_with("$ORIGIN/") {
                    return Err(format!("ELF RPATH escapes the artifact directory: {entry}"));
                }
            }
        }
    }
    validate_exports(parse_nm_exports(exports))
}

fn validate_macos(library: &Path, expected_architecture: &str) -> Result<(), String> {
    let architectures = capture("lipo", [OsStr::new("-archs"), library.as_os_str()])?;
    let identity = capture("otool", [OsStr::new("-D"), library.as_os_str()])?;
    let dependencies = capture("otool", [OsStr::new("-L"), library.as_os_str()])?;
    let load_commands = capture("otool", [OsStr::new("-l"), library.as_os_str()])?;
    let exports = capture("nm", [OsStr::new("-gU"), library.as_os_str()])?;
    validate_macos_output(
        &architectures,
        &identity,
        &dependencies,
        &load_commands,
        &exports,
        expected_architecture,
    )
}

fn validate_macos_output(
    architectures: &str,
    identity: &str,
    dependencies: &str,
    load_commands: &str,
    exports: &str,
    expected_architecture: &str,
) -> Result<(), String> {
    require_architecture(architectures, expected_architecture, "x86_64", "arm64")?;
    if !identity
        .lines()
        .skip(1)
        .any(|line| line.trim() == "@rpath/libopenslide.1.dylib")
    {
        return Err("Mach-O install ID must be @rpath/libopenslide.1.dylib".into());
    }
    for dependency in dependencies
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
    {
        if dependency == "@rpath/libopenslide.1.dylib" {
            continue;
        }
        if !(dependency.starts_with("/usr/lib/")
            || dependency.starts_with("/System/Library/")
            || dependency.starts_with("@rpath/")
            || dependency.starts_with("@loader_path/"))
        {
            return Err(format!(
                "Mach-O dependency has a nonportable path: {dependency}"
            ));
        }
    }
    let mut lines = load_commands.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "cmd LC_RPATH" {
            let path_line = lines
                .by_ref()
                .find(|candidate| candidate.trim_start().starts_with("path "))
                .ok_or_else(|| "Mach-O LC_RPATH has no path".to_string())?;
            let rpath = path_line.split_whitespace().nth(1).unwrap_or_default();
            if !(rpath.starts_with("@loader_path") || rpath.starts_with("@executable_path")) {
                return Err(format!("Mach-O RPATH has a nonportable path: {rpath}"));
            }
        }
    }
    validate_exports(parse_nm_exports(exports))
}

fn validate_windows(library: &Path, expected_architecture: &str) -> Result<(), String> {
    let headers = capture("dumpbin", [OsStr::new("/headers"), library.as_os_str()])?;
    let dependencies = capture("dumpbin", [OsStr::new("/dependents"), library.as_os_str()])?;
    let exports = capture("dumpbin", [OsStr::new("/exports"), library.as_os_str()])?;
    validate_windows_output(&headers, &dependencies, &exports, expected_architecture)
}

fn validate_windows_output(
    headers: &str,
    dependencies: &str,
    exports: &str,
    expected_architecture: &str,
) -> Result<(), String> {
    require_architecture(
        headers,
        expected_architecture,
        "machine (x64)",
        "machine (ARM64)",
    )?;
    for dependency in dependencies
        .lines()
        .map(str::trim)
        .filter(|line| line.to_ascii_lowercase().ends_with(".dll"))
    {
        if dependency.contains(['/', '\\']) {
            return Err(format!(
                "PE dependency has a path instead of an import name: {dependency}"
            ));
        }
    }
    validate_exports(parse_dumpbin_exports(exports))
}

fn require_architecture(
    output: &str,
    expected: &str,
    x64_marker: &str,
    arm64_marker: &str,
) -> Result<(), String> {
    let output = output.to_ascii_lowercase();
    let marker = match expected {
        "x86_64" | "x64" | "amd64" => x64_marker,
        "aarch64" | "arm64" => arm64_marker,
        other => {
            return Err(format!(
                "unsupported expected artifact architecture `{other}`"
            ))
        }
    };
    if output.contains(&marker.to_ascii_lowercase()) {
        Ok(())
    } else {
        Err(format!(
            "artifact is not the expected {expected} architecture"
        ))
    }
}

fn parse_nm_exports(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .map(|name| name.strip_prefix('_').unwrap_or(name))
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_dumpbin_exports(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.len() >= 4
                && fields[0].parse::<u32>().is_ok()
                && fields[1].chars().all(|ch| ch.is_ascii_hexdigit())
                && fields[2].chars().all(|ch| ch.is_ascii_hexdigit()))
            .then(|| fields[3].to_string())
        })
        .collect()
}

fn validate_exports(actual: BTreeSet<String>) -> Result<(), String> {
    let expected = OFFICIAL_EXPORTS
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    if actual == expected {
        return Ok(());
    }
    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
    let unexpected = actual.difference(&expected).cloned().collect::<Vec<_>>();
    Err(format!(
        "OpenSlide export allowlist mismatch; missing: {missing:?}; unexpected: {unexpected:?}"
    ))
}

fn bracket_value(line: &str) -> Option<&str> {
    line.split_once('[')?
        .1
        .split_once(']')
        .map(|(value, _)| value)
}

fn compile_and_run_smoke(
    library: &Path,
    include_dir: &Path,
    platform: ArtifactPlatform,
) -> Result<(), String> {
    let scratch = env::temp_dir().join(format!(
        "wsi-rs-openslide-artifact-smoke-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|err| format!("read system clock: {err}"))?
            .as_nanos()
    ));
    fs::create_dir(&scratch)
        .map_err(|err| format!("create smoke directory {}: {err}", scratch.display()))?;
    let source = scratch.join("openslide-smoke.c");
    fs::write(&source, smoke_source(platform))
        .map_err(|err| format!("write C smoke source {}: {err}", source.display()))?;
    let executable = scratch.join(if matches!(platform, ArtifactPlatform::Windows) {
        "openslide-smoke.exe"
    } else {
        "openslide-smoke"
    });
    match platform {
        ArtifactPlatform::Windows => {
            let args = vec![
                OsString::from("/nologo"),
                OsString::from("/W4"),
                OsString::from("/WX"),
                OsString::from(format!("/I{}", include_dir.display())),
                source.as_os_str().to_owned(),
                OsString::from(format!("/Fe:{}", executable.display())),
            ];
            run("cl", &args)?;
            run(executable.as_os_str(), &[library.as_os_str().to_owned()])
        }
        ArtifactPlatform::MacOS | ArtifactPlatform::Linux => {
            let parent = library
                .parent()
                .ok_or_else(|| "artifact path has no parent".to_string())?;
            let args = vec![
                OsString::from("-std=c11"),
                OsString::from("-Wall"),
                OsString::from("-Wextra"),
                OsString::from("-Werror"),
                OsString::from(format!("-I{}", include_dir.display())),
                source.as_os_str().to_owned(),
                library.as_os_str().to_owned(),
                OsString::from(format!("-Wl,-rpath,{}", parent.display())),
                OsString::from("-o"),
                executable.as_os_str().to_owned(),
            ];
            run("cc", &args)?;
            run(executable.as_os_str(), &[])
        }
    }
}

fn smoke_source(platform: ArtifactPlatform) -> &'static str {
    if matches!(platform, ArtifactPlatform::Windows) {
        r#"#include <windows.h>
#include "openslide.h"
#include <string.h>
typedef const char *(__cdecl *version_function)(void);
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    HMODULE library = LoadLibraryA(argv[1]);
    if (library == NULL) return 3;
    union { FARPROC address; version_function function; } version;
    version.address = GetProcAddress(library, "openslide_get_version");
    if (version.address == NULL) return 4;
    const char *value = version.function();
    if (value == NULL || strncmp(value, "OpenSlide 4.0.1+wsi-rs-", 23) != 0) return 5;
    FreeLibrary(library);
    return 0;
}
"#
    } else {
        r#"#include "openslide.h"
#include <string.h>
int main(void) {
    const char *version = openslide_get_version();
    if (version == NULL || strncmp(version, "OpenSlide 4.0.1+wsi-rs-", 23) != 0) return 2;
    openslide_cache_t *cache = openslide_cache_create(4096);
    if (cache == NULL) return 3;
    openslide_cache_release(cache);
    return 0;
}
"#
    }
}

fn capture<I, S>(program: &str, arguments: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|err| format!("failed to start `{program}`: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "`{program}` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn run(program: impl AsRef<OsStr>, arguments: &[OsString]) -> Result<(), String> {
    let program = program.as_ref();
    let status = Command::new(program)
        .args(arguments)
        .status()
        .map_err(|err| format!("failed to start `{}`: {err}", program.to_string_lossy()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{}` exited with {status}",
            program.to_string_lossy()
        ))
    }
}

#[cfg(test)]
mod tests;
