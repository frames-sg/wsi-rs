use super::*;
use serde_json::json;

fn write_metadata(path: &Path, artifact: &str, sha256: &str) {
    let metadata = json!({
        "schema_version": 1,
        "artifact": artifact,
        "target": "linux-x64",
        "signed": false,
        "gpu_feature": "cuda",
        "gpu_runtime_required": false,
        "sha256": sha256,
    });
    fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
}

#[test]
fn artifact_metadata_sha256_must_match_library_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("libopenslide.so.1");
    let metadata = directory.path().join("artifact-metadata.json");
    fs::write(&library, b"reviewed artifact bytes").unwrap();
    let digest = wsi_rs_perf::sha256_file(&library).unwrap();
    write_metadata(&metadata, "libopenslide.so.1", &digest);
    validate_metadata(&library, &metadata).unwrap();

    fs::write(&library, b"different artifact bytes").unwrap();
    let error = validate_metadata(&library, &metadata).unwrap_err();
    assert!(error.contains("SHA-256 mismatch"), "{error}");
    assert!(error.contains(&digest), "{error}");
}

#[test]
fn artifact_metadata_sha256_comparison_accepts_uppercase_hex() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("libopenslide.so.1");
    let metadata = directory.path().join("artifact-metadata.json");
    fs::write(&library, b"artifact bytes").unwrap();
    let digest = wsi_rs_perf::sha256_file(&library).unwrap().to_uppercase();
    write_metadata(&metadata, "libopenslide.so.1", &digest);

    validate_metadata(&library, &metadata).unwrap();
}

#[test]
fn artifact_smoke_validates_arguments_paths_and_platform_names() {
    assert!(smoke(Vec::new()).unwrap_err().contains("requires"));

    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("libopenslide.so.1");
    let metadata = directory.path().join("metadata.json");
    let error = smoke_paths(&missing, directory.path(), "x86_64", &metadata).unwrap_err();
    assert!(error.contains("artifact is missing"), "{error}");

    fs::write(&missing, b"not an ELF library").unwrap();
    let error = smoke_paths(&missing, directory.path(), "x86_64", &metadata).unwrap_err();
    assert!(error.contains("header is missing"), "{error}");

    assert!(matches!(
        platform_for_library(Path::new("libopenslide.1.dylib")),
        Ok(ArtifactPlatform::MacOS)
    ));
    assert!(matches!(
        platform_for_library(Path::new("libopenslide.so.1")),
        Ok(ArtifactPlatform::Linux)
    ));
    assert!(matches!(
        platform_for_library(Path::new("libopenslide-1.dll")),
        Ok(ArtifactPlatform::Windows)
    ));
    assert!(platform_for_library(Path::new("libopenslide.so")).is_err());
    assert!(platform_for_library(Path::new("/")).is_err());
}

#[test]
fn artifact_metadata_rejects_each_release_contract_violation() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("libopenslide.so.1");
    let metadata = directory.path().join("metadata.json");
    fs::write(&library, b"artifact").unwrap();
    let digest = wsi_rs_perf::sha256_file(&library).unwrap();

    assert!(validate_metadata(&library, &metadata)
        .unwrap_err()
        .contains("read artifact metadata"));
    fs::write(&metadata, b"not json").unwrap();
    assert!(validate_metadata(&library, &metadata)
        .unwrap_err()
        .contains("parse artifact metadata"));

    let base = json!({
        "schema_version": 1,
        "artifact": "libopenslide.so.1",
        "target": "linux-x64",
        "signed": false,
        "gpu_feature": "cuda",
        "gpu_runtime_required": false,
        "sha256": digest,
    });
    for (field, value, expected) in [
        ("schema_version", json!(2), "schema or artifact name"),
        ("artifact", json!("wrong.so"), "schema or artifact name"),
        ("signed", json!(true), "explicitly unsigned"),
        ("target", json!(""), "must be nonempty"),
        ("gpu_feature", json!(""), "must be nonempty"),
        ("sha256", json!("xyz"), "SHA-256 is malformed"),
        ("gpu_runtime_required", json!(true), "dynamically fall back"),
    ] {
        let mut candidate = base.clone();
        candidate[field] = value;
        fs::write(&metadata, serde_json::to_vec(&candidate).unwrap()).unwrap();
        let error = validate_metadata(&library, &metadata).unwrap_err();
        assert!(error.contains(expected), "{field}: {error}");
    }
}

#[test]
fn artifact_text_parsers_and_architecture_checks_fail_closed() {
    assert!(require_architecture("Machine: X86-64", "x86_64", "X86-64", "AArch64").is_ok());
    assert!(require_architecture(
        "machine (ARM64)",
        "arm64",
        "machine (x64)",
        "machine (ARM64)"
    )
    .is_ok());
    assert!(require_architecture("Machine: X86-64", "mips", "X86-64", "AArch64").is_err());
    assert!(require_architecture("Machine: AArch64", "x86_64", "X86-64", "AArch64").is_err());

    assert_eq!(bracket_value("tag [value] suffix"), Some("value"));
    assert_eq!(bracket_value("tag without brackets"), None);
    assert_eq!(
        parse_nm_exports("0000 T _openslide_open\nnoise\n")
            .into_iter()
            .collect::<Vec<_>>(),
        vec!["noise", "openslide_open"]
    );
    assert_eq!(
        parse_dumpbin_exports("1 00000000 00000000 openslide_open\ninvalid row")
            .into_iter()
            .collect::<Vec<_>>(),
        vec!["openslide_open"]
    );
    assert!(validate_exports(BTreeSet::new())
        .unwrap_err()
        .contains("missing"));
    let expected = OFFICIAL_EXPORTS.into_iter().map(str::to_owned).collect();
    validate_exports(expected).unwrap();

    assert!(smoke_source(ArtifactPlatform::Windows).contains("LoadLibraryA"));
    assert!(smoke_source(ArtifactPlatform::Linux).contains("openslide_cache_create"));
}

#[test]
fn artifact_platform_output_validation_covers_portable_and_rejected_paths() {
    let nm_exports = OFFICIAL_EXPORTS
        .into_iter()
        .map(|name| format!("00000000 T _{name}"))
        .collect::<Vec<_>>()
        .join("\n");
    let dumpbin_exports = OFFICIAL_EXPORTS
        .into_iter()
        .enumerate()
        .map(|(index, name)| format!("{} 00000000 00000000 {name}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");

    let linux_dynamic = "Library soname: [libopenslide.so.1]\nShared library: [libc.so.6]\n(RUNPATH) Library runpath: [$ORIGIN:$ORIGIN/lib]\n";
    validate_linux_output("Machine: X86-64", linux_dynamic, &nm_exports, "x86_64").unwrap();
    assert!(validate_linux_output(
        "Machine: X86-64",
        "Shared library: [libc.so.6]",
        &nm_exports,
        "x86_64",
    )
    .unwrap_err()
    .contains("SONAME"));
    assert!(validate_linux_output(
        "Machine: X86-64",
        "Library soname: [libopenslide.so.1]\nShared library: [/tmp/libcodec.so]",
        &nm_exports,
        "x86_64",
    )
    .unwrap_err()
    .contains("invalid ELF dependency"));
    assert!(validate_linux_output(
        "Machine: X86-64",
        "Library soname: [libopenslide.so.1]\n(RPATH) Library rpath: [/tmp/lib]",
        &nm_exports,
        "x86_64",
    )
    .unwrap_err()
    .contains("RPATH escapes"));

    let identity = "artifact:\n@rpath/libopenslide.1.dylib\n";
    let dependencies = "artifact:\n@rpath/libopenslide.1.dylib (compatibility)\n/usr/lib/libSystem.B.dylib (compatibility)\n@loader_path/libcodec.dylib (compatibility)\n";
    let load_commands = "cmd LC_RPATH\ncmdsize 32\npath @loader_path/lib (offset 12)\n";
    validate_macos_output(
        "x86_64",
        identity,
        dependencies,
        load_commands,
        &nm_exports,
        "x86_64",
    )
    .unwrap();
    assert!(validate_macos_output(
        "x86_64",
        "artifact:\n/tmp/libopenslide.1.dylib\n",
        dependencies,
        load_commands,
        &nm_exports,
        "x86_64",
    )
    .unwrap_err()
    .contains("install ID"));
    assert!(validate_macos_output(
        "x86_64",
        identity,
        "artifact:\n/tmp/libcodec.dylib (compatibility)\n",
        load_commands,
        &nm_exports,
        "x86_64",
    )
    .unwrap_err()
    .contains("nonportable path"));
    assert!(validate_macos_output(
        "x86_64",
        identity,
        dependencies,
        "cmd LC_RPATH\ncmdsize 32\n",
        &nm_exports,
        "x86_64",
    )
    .unwrap_err()
    .contains("has no path"));
    assert!(validate_macos_output(
        "x86_64",
        identity,
        dependencies,
        "cmd LC_RPATH\npath /tmp/lib (offset 12)\n",
        &nm_exports,
        "x86_64",
    )
    .unwrap_err()
    .contains("RPATH has a nonportable path"));

    validate_windows_output(
        "machine (x64)",
        "KERNEL32.dll\nVCRUNTIME140.dll\n",
        &dumpbin_exports,
        "x86_64",
    )
    .unwrap();
    assert!(validate_windows_output(
        "machine (x64)",
        "C:\\runtime\\codec.dll\n",
        &dumpbin_exports,
        "x86_64",
    )
    .unwrap_err()
    .contains("path instead of an import name"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_artifact_smoke_accepts_a_canonical_self_contained_library() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("libopenslide.so.1");
    let include_dir = directory.path().join("include");
    let metadata = directory.path().join("artifact-metadata.json");
    let source = directory.path().join("openslide-shim.c");
    fs::create_dir(&include_dir).unwrap();
    fs::write(
        include_dir.join("openslide.h"),
        r#"#include <stddef.h>
typedef void openslide_cache_t;
const char *openslide_get_version(void);
openslide_cache_t *openslide_cache_create(size_t capacity);
void openslide_cache_release(openslide_cache_t *cache);
"#,
    )
    .unwrap();
    fs::write(include_dir.join("openslide-features.h"), "").unwrap();

    let mut definitions = String::from(
        r#"#include <stddef.h>
#define EXPORT __attribute__((visibility("default")))
static int CACHE;
EXPORT const char *openslide_get_version(void) { return "OpenSlide 4.0.1+wsi-rs-test"; }
EXPORT void *openslide_cache_create(size_t capacity) { (void)capacity; return &CACHE; }
EXPORT void openslide_cache_release(void *cache) { (void)cache; }
"#,
    );
    for export in OFFICIAL_EXPORTS {
        if !matches!(
            export,
            "openslide_get_version" | "openslide_cache_create" | "openslide_cache_release"
        ) {
            definitions.push_str(&format!("EXPORT void {export}(void) {{}}\n"));
        }
    }
    fs::write(&source, definitions).unwrap();
    run(
        "cc",
        &[
            OsString::from("-shared"),
            OsString::from("-fPIC"),
            OsString::from("-fvisibility=hidden"),
            OsString::from("-Wl,-soname,libopenslide.so.1"),
            source.as_os_str().to_owned(),
            OsString::from("-o"),
            library.as_os_str().to_owned(),
        ],
    )
    .unwrap();
    let digest = wsi_rs_perf::sha256_file(&library).unwrap();
    write_metadata(&metadata, "libopenslide.so.1", &digest);

    smoke_paths(&library, &include_dir, std::env::consts::ARCH, &metadata).unwrap();
}

#[cfg(unix)]
#[test]
fn artifact_process_helpers_report_success_exit_and_spawn_failures() {
    assert!(capture("rustc", ["--version"]).unwrap().contains("rustc"));
    assert!(capture("rustc", ["--definitely-invalid"]).is_err());
    assert!(capture(
        "wsi-rs-command-that-does-not-exist",
        std::iter::empty::<&str>()
    )
    .is_err());

    run("rustc", &[OsString::from("--version")]).unwrap();
    assert!(run("rustc", &[OsString::from("--definitely-invalid")]).is_err());
    assert!(run("wsi-rs-command-that-does-not-exist", &[]).is_err());
}
