use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use wsi_rs_openslide_shim::install::{
    execute_install, execute_restore, install_destinations, manifest_path, PlatformLibraryNames,
};

fn built_shim_library() -> PathBuf {
    let test_binary = std::env::current_exe().expect("current test binary");
    let deps = test_binary.parent().expect("test binary directory");
    let library = deps.join(format!(
        "{}wsi_rs_openslide_shim{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    assert!(
        library.is_file(),
        "built shim missing: {}",
        library.display()
    );
    library
}

fn run_installer(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wsi-rs-openslide-install"))
        .args(arguments)
        .output()
        .expect("run installer CLI")
}

#[test]
fn install_destinations_include_all_loader_compatible_names() {
    let mac = PlatformLibraryNames::MacOS;
    assert_eq!(mac.names(), &["libopenslide.1.dylib", "libopenslide.dylib"]);

    let linux = PlatformLibraryNames::Linux;
    assert_eq!(linux.names(), &["libopenslide.so.1", "libopenslide.so"]);

    let windows = PlatformLibraryNames::Windows;
    assert_eq!(windows.names(), &["libopenslide-1.dll"]);

    let destinations = install_destinations(Path::new("/prefix"), mac);
    assert_eq!(
        destinations[0],
        Path::new("/prefix/lib/libopenslide.1.dylib")
    );
    assert_eq!(destinations.len(), 2);
}

#[test]
fn failed_verification_rolls_back_every_destination() {
    let temp = tempfile::tempdir().expect("temp directory");
    let prefix = temp.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("library directory");
    let destinations = install_destinations(&prefix, PlatformLibraryNames::Linux);
    for (index, destination) in destinations.iter().enumerate() {
        std::fs::write(destination, format!("original-{index}")).expect("write original library");
    }
    let shim = temp.path().join("invalid-shim.so");
    std::fs::write(&shim, b"not a dynamic library").expect("write invalid shim");

    let error = execute_install(&prefix, &shim, PlatformLibraryNames::Linux, 42)
        .expect_err("verification must fail");

    assert!(error.to_string().contains("rolled back"), "{error}");
    for (index, destination) in destinations.iter().enumerate() {
        assert_eq!(
            std::fs::read_to_string(destination).expect("restored original"),
            format!("original-{index}")
        );
        assert!(!PathBuf::from(format!("{}.wsi_rs-backup-42", destination.display())).exists());
        assert!(!PathBuf::from(format!("{}.wsi_rs-stage-42", destination.display())).exists());
    }
    assert!(!manifest_path(&prefix).exists());
}

#[test]
fn successful_install_and_restore_round_trip_preserves_original_libraries() {
    let temp = tempfile::tempdir().expect("temp directory");
    let prefix = temp.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("library directory");
    let platform = PlatformLibraryNames::current().expect("supported test platform");
    let destinations = install_destinations(&prefix, platform);
    for (index, destination) in destinations.iter().enumerate() {
        std::fs::write(destination, format!("original-{index}")).expect("write original library");
    }

    let manifest = execute_install(&prefix, &built_shim_library(), platform, 71)
        .expect("install and verify built shim");

    assert_eq!(
        manifest
            .canonicalize()
            .expect("canonical installed manifest"),
        manifest_path(&prefix)
            .canonicalize()
            .expect("canonical expected manifest")
    );
    assert!(manifest.is_file());
    assert!(std::fs::read_to_string(&manifest)
        .expect("read installed manifest")
        .starts_with("wsi-rs-openslide-shim\t1\tinstalled\n"));
    for destination in &destinations {
        assert!(destination.is_file());
        assert!(PathBuf::from(format!("{}.wsi_rs-backup-71", destination.display())).is_file());
    }

    execute_restore(&prefix, 72).expect("restore original libraries");

    assert!(!manifest.exists());
    for (index, destination) in destinations.iter().enumerate() {
        assert_eq!(
            std::fs::read_to_string(destination).expect("restored original library"),
            format!("original-{index}")
        );
        assert!(!PathBuf::from(format!("{}.wsi_rs-backup-71", destination.display())).exists());
        assert!(!PathBuf::from(format!("{}.wsi_rs-removed-72", destination.display())).exists());
    }
}

#[test]
fn restore_rejects_manifest_paths_outside_prefix() {
    let temp = tempfile::tempdir().expect("temp directory");
    let prefix = temp.path().join("prefix");
    std::fs::create_dir_all(prefix.join("lib")).expect("library directory");
    let outside = temp.path().join("outside.so");
    std::fs::write(&outside, b"outside").expect("outside file");
    std::fs::write(
        manifest_path(&prefix),
        format!(
            "wsi-rs-openslide-shim\t1\tinstalled\n{}\t\n",
            outside.display()
        ),
    )
    .expect("malicious manifest");

    let error = execute_restore(&prefix, 9).expect_err("outside path must be rejected");
    assert!(
        error.to_string().contains("outside the supported prefix"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(&outside).expect("outside remains"),
        b"outside"
    );
}

#[test]
fn restore_rejects_oversized_manifest_before_parsing_entries() {
    let temp = tempfile::tempdir().expect("temp directory");
    let prefix = temp.path().join("prefix");
    std::fs::create_dir_all(prefix.join("lib")).expect("library directory");
    let mut manifest = b"wsi-rs-openslide-shim\t1\tinstalled\n".to_vec();
    manifest.resize(64 * 1024 + 1, b'x');
    std::fs::write(manifest_path(&prefix), manifest).expect("oversized manifest");

    let error = execute_restore(&prefix, 9).expect_err("oversized manifest must be rejected");
    assert!(error.contains("65536 byte safety limit"), "{error}");
}

#[cfg(unix)]
#[test]
fn install_rejects_symlink_destinations_before_mutation() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp directory");
    let prefix = temp.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("library directory");
    let outside = temp.path().join("outside.so");
    std::fs::write(&outside, b"outside").expect("outside file");
    symlink(&outside, lib.join("libopenslide.so.1")).expect("destination symlink");
    let shim = temp.path().join("shim.so");
    std::fs::write(&shim, b"shim").expect("shim file");

    let error = execute_install(&prefix, &shim, PlatformLibraryNames::Linux, 4)
        .expect_err("symlink must be rejected");
    assert!(
        error.to_string().contains("must not be a symlink"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(&outside).expect("outside remains"),
        b"outside"
    );
}

#[cfg(unix)]
#[test]
fn install_rejects_broken_stage_symlink_without_writing_its_target() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp directory");
    let prefix = temp.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("library directory");
    let outside = temp.path().join("outside-stage-target.so");
    let stage = lib.join("libopenslide.so.1.wsi_rs-stage-5");
    symlink(&outside, &stage).expect("broken stage symlink");
    let shim = temp.path().join("shim.so");
    std::fs::write(&shim, b"shim").expect("shim file");

    let error = execute_install(&prefix, &shim, PlatformLibraryNames::Linux, 5)
        .expect_err("pre-existing stage path must be rejected");

    assert!(error.contains("stage path already exists"), "{error}");
    assert!(
        !outside.exists(),
        "broken symlink target must not be created"
    );
    assert!(
        stage.is_symlink(),
        "pre-existing stage link must be preserved"
    );
}

#[cfg(unix)]
#[test]
fn install_rejects_broken_manifest_temp_symlink_without_writing_its_target() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp directory");
    let prefix = temp.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("library directory");
    let outside = temp.path().join("outside-manifest-target.tsv");
    let temporary_manifest = lib.join(".wsi-rs-openslide-shim-install.tsv.tmp");
    symlink(&outside, &temporary_manifest).expect("broken manifest-temp symlink");
    let shim = temp.path().join("shim.so");
    std::fs::write(&shim, b"shim").expect("shim file");

    let error = execute_install(&prefix, &shim, PlatformLibraryNames::Linux, 6)
        .expect_err("pre-existing manifest temp path must be rejected");

    assert!(error.contains("already exists"), "{error}");
    assert!(
        !outside.exists(),
        "broken symlink target must not be created"
    );
    assert!(
        temporary_manifest.is_symlink(),
        "pre-existing manifest-temp link must be preserved"
    );
}

#[cfg(unix)]
#[test]
fn restore_rejects_symlink_backup_before_mutation() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp directory");
    let prefix = temp.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("library directory");
    let canonical_lib = lib.canonicalize().expect("canonical library directory");
    let destination = canonical_lib.join("libopenslide.so.1");
    std::fs::write(&destination, b"installed-shim").expect("installed shim");
    let outside = temp.path().join("outside-original.so");
    std::fs::write(&outside, b"outside-original").expect("outside file");
    let backup = canonical_lib.join("libopenslide.so.1.wsi_rs-backup-41");
    symlink(&outside, &backup).expect("backup symlink");
    std::fs::write(
        manifest_path(&prefix),
        format!(
            "wsi-rs-openslide-shim\t1\tinstalled\n{}\t{}\n",
            destination.display(),
            backup.display()
        ),
    )
    .expect("forged manifest");

    let error = execute_restore(&prefix, 42).expect_err("backup symlink must be rejected");

    assert!(error.contains("must not be a symlink"), "{error}");
    assert_eq!(
        std::fs::read(&destination).expect("installed shim remains"),
        b"installed-shim"
    );
    assert_eq!(
        std::fs::read(&outside).expect("outside remains"),
        b"outside-original"
    );
    assert!(backup.is_symlink());
}

#[test]
fn installer_cli_reports_each_argument_and_operation_error_with_usage() {
    let temp = tempfile::tempdir().expect("temporary CLI directory");
    let prefix = temp.path().join("prefix");
    let prefix_text = prefix.to_str().expect("UTF-8 temporary prefix");

    let cases: &[(&[&str], &str, bool)] = &[
        (&[], "missing command", true),
        (&["install", "--prefix"], "--prefix requires a path", true),
        (&["install", "--shim"], "--shim requires a path", true),
        (&["install", "--help"], "usage:", true),
        (&["unknown"], "unknown command: unknown", true),
        (
            &["restore", "--unknown"],
            "unknown argument: --unknown",
            true,
        ),
        (
            &["install", "--prefix", prefix_text],
            "install requires --shim",
            true,
        ),
        (
            &[
                "install",
                "--shim",
                "/definitely/missing/wsi-rs-shim",
                "--prefix",
                prefix_text,
            ],
            "shim library does not exist",
            false,
        ),
        (
            &["restore", "--prefix", prefix_text],
            "restore manifest",
            false,
        ),
    ];

    for (arguments, expected, expects_usage) in cases {
        let output = run_installer(arguments);
        assert!(
            !output.status.success(),
            "arguments unexpectedly passed: {arguments:?}"
        );
        let stderr = String::from_utf8(output.stderr).expect("UTF-8 CLI stderr");
        assert!(stderr.contains(expected), "unexpected stderr: {stderr}");
        assert_eq!(
            stderr.contains("usage:"),
            *expects_usage,
            "unexpected usage handling for {arguments:?}: {stderr}"
        );
    }
}

#[test]
fn installer_cli_round_trip_restores_every_original_library() {
    let temp = tempfile::tempdir().expect("temporary CLI install directory");
    let prefix = temp.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("library directory");
    let platform = PlatformLibraryNames::current().expect("supported test platform");
    let destinations = install_destinations(&prefix, platform);
    for (index, destination) in destinations.iter().enumerate() {
        std::fs::write(destination, format!("cli-original-{index}"))
            .expect("write original library");
    }

    let shim = built_shim_library();
    let install = run_installer(&[
        "install",
        "--shim",
        shim.to_str().expect("UTF-8 shim path"),
        "--prefix",
        prefix.to_str().expect("UTF-8 prefix path"),
    ]);
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(String::from_utf8_lossy(&install.stdout).contains("restore manifest"));
    assert!(manifest_path(&prefix).is_file());

    let restore = run_installer(&[
        "restore",
        "--prefix",
        prefix.to_str().expect("UTF-8 prefix path"),
    ]);
    assert!(
        restore.status.success(),
        "restore failed: {}",
        String::from_utf8_lossy(&restore.stderr)
    );
    assert!(String::from_utf8_lossy(&restore.stdout).contains("restored OpenSlide libraries"));

    for (index, destination) in destinations.iter().enumerate() {
        assert_eq!(
            std::fs::read_to_string(destination).expect("restored original library"),
            format!("cli-original-{index}")
        );
    }
    assert!(!manifest_path(&prefix).exists());
}
