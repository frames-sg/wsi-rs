use std::path::{Path, PathBuf};

use wsi_rs_openslide_shim::install::{
    execute_install, execute_restore, install_destinations, manifest_path, plan_install,
    InstallStep, PlatformLibraryNames,
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

#[test]
fn install_destinations_include_all_loader_compatible_names() {
    let mac = PlatformLibraryNames::MacOS;
    assert_eq!(
        mac.names(),
        [
            "libopenslide.1.dylib",
            "libopenslide.dylib",
            "libopenslide.4.dylib"
        ]
    );

    let linux = PlatformLibraryNames::Linux;
    assert_eq!(
        linux.names(),
        ["libopenslide.so.1", "libopenslide.so", "libopenslide.so.4"]
    );

    let destinations = install_destinations(Path::new("/prefix"), mac);
    assert_eq!(
        destinations[0],
        Path::new("/prefix/lib/libopenslide.1.dylib")
    );
    assert_eq!(
        destinations[2],
        Path::new("/prefix/lib/libopenslide.4.dylib")
    );
}

#[test]
fn install_plan_backs_up_existing_destinations_before_copying_shim() {
    let prefix = Path::new("/prefix");
    let shim = Path::new("/build/libwsi_rs_openslide_shim.dylib");
    let steps = plan_install(prefix, shim, PlatformLibraryNames::MacOS, 42, |path| {
        path.ends_with("libopenslide.1.dylib")
    });

    assert_eq!(
        steps,
        vec![
            InstallStep::Backup {
                from: Path::new("/prefix/lib/libopenslide.1.dylib").to_path_buf(),
                to: Path::new("/prefix/lib/libopenslide.1.dylib.wsi_rs-backup-42").to_path_buf(),
            },
            InstallStep::CopyShim {
                from: shim.to_path_buf(),
                to: Path::new("/prefix/lib/libopenslide.1.dylib").to_path_buf(),
            },
            InstallStep::CopyShim {
                from: shim.to_path_buf(),
                to: Path::new("/prefix/lib/libopenslide.dylib").to_path_buf(),
            },
            InstallStep::CopyShim {
                from: shim.to_path_buf(),
                to: Path::new("/prefix/lib/libopenslide.4.dylib").to_path_buf(),
            },
        ]
    );
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

    assert!(error.contains("rolled back"), "{error}");
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
    assert!(error.contains("outside the supported prefix"), "{error}");
    assert_eq!(
        std::fs::read(&outside).expect("outside remains"),
        b"outside"
    );
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
    assert!(error.contains("must not be a symlink"), "{error}");
    assert_eq!(
        std::fs::read(&outside).expect("outside remains"),
        b"outside"
    );
}
