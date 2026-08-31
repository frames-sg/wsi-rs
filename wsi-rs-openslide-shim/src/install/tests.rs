use super::*;

#[test]
fn rollback_failure_is_typed_and_preserves_recoverable_backup() {
    let directory = tempfile::tempdir().expect("temporary install directory");
    let destination = directory.path().join("libopenslide.so");
    let backup = directory.path().join("libopenslide.so.wsi_rs-backup-42");
    let stage = directory.path().join("libopenslide.so.wsi_rs-stage-42");
    let manifest = directory.path().join("install.tsv");
    std::fs::write(&destination, b"installed shim").expect("installed shim");
    std::fs::write(&backup, b"original library").expect("preserved original");
    std::fs::write(&manifest, b"prepared journal").expect("recovery journal");
    let entries = vec![RestoreEntry {
        destination: destination.clone(),
        backup: Some(backup.clone()),
    }];

    let rollback = rollback_install_with(
        &entries,
        &[stage],
        &manifest,
        |path| std::fs::remove_file(path),
        |_from, _to| Err(std::io::Error::other("injected restore failure")),
    )
    .expect_err("injected restoration failure must be observable");
    let error = combined_install_error(
        "injected primary install failure".into(),
        rollback,
        &entries,
        &manifest,
    );

    let rendered = error.to_string();
    assert!(rendered.contains("injected primary install failure"));
    assert!(rendered.contains("injected restore failure"));
    assert!(rendered.contains(&backup.display().to_string()));

    let InstallError::RollbackFailed {
        primary,
        rollback,
        preserved_backups,
        recovery_manifest,
    } = error
    else {
        panic!("expected typed rollback failure");
    };
    assert!(primary.contains("primary install failure"));
    assert!(rollback.contains("injected restore failure"));
    assert_eq!(preserved_backups, vec![backup.clone()]);
    assert_eq!(recovery_manifest, manifest);
    assert_eq!(
        std::fs::read(&backup).expect("recoverable backup remains"),
        b"original library"
    );
    assert!(
        recovery_manifest.exists(),
        "journal must remain for recovery"
    );
}

#[test]
fn manifest_parser_rejects_each_untrusted_text_boundary() {
    let directory = tempfile::tempdir().expect("temporary manifest directory");
    let manifest = directory.path().join("install.tsv");
    let cases: &[(&[u8], &str)] = &[
        (b"", "manifest is empty"),
        (&[0xff, 0xfe], "as UTF-8"),
        (b"wrong\t1\tinstalled\n", "manifest header is invalid"),
        (
            b"wsi-rs-openslide-shim\t2\tinstalled\n",
            "manifest version or state is invalid",
        ),
        (
            b"wsi-rs-openslide-shim\t1\tunknown\n",
            "manifest version or state is invalid",
        ),
        (
            b"wsi-rs-openslide-shim\t1\tinstalled\nmalformed\n",
            "manifest line 2 is malformed",
        ),
        (
            b"wsi-rs-openslide-shim\t1\tinstalled\na\t\nb\t\nc\t\nd\t\n",
            "manifest has more than 3 entries",
        ),
    ];

    for (contents, expected) in cases {
        std::fs::write(&manifest, contents).expect("write malformed manifest");
        let error = read_manifest(&manifest).expect_err("malformed manifest must fail");
        assert!(error.contains(expected), "unexpected error: {error}");
    }

    let missing = directory.path().join("missing.tsv");
    let error = read_manifest(&missing).expect_err("missing manifest must fail");
    assert!(error.contains("open"), "unexpected error: {error}");

    let error = read_manifest(directory.path()).expect_err("directory read must fail");
    assert!(error.contains("read"), "unexpected error: {error}");
}

#[test]
fn install_preflight_rejects_missing_inputs_and_side_path_collisions() {
    let directory = tempfile::tempdir().expect("temporary install directory");
    let prefix = directory.path().join("prefix");
    let shim = directory.path().join("shim.so");
    std::fs::write(&shim, b"not reached by preflight cases").expect("write shim fixture");

    let missing = directory.path().join("missing.so");
    let error = execute_install_detailed(&prefix, &missing, PlatformLibraryNames::Linux, 10)
        .expect_err("missing shim must fail");
    assert!(error.to_string().contains("does not exist"));

    let bad_prefix = directory.path().join("prefix-file");
    std::fs::write(&bad_prefix, b"not a directory").expect("write prefix file");
    let error = execute_install_detailed(&bad_prefix, &shim, PlatformLibraryNames::Linux, 11)
        .expect_err("file prefix must fail");
    assert!(error.to_string().contains("create"));

    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("create library directory");
    std::fs::write(manifest_path(&prefix), b"existing journal").expect("write journal");
    let error = execute_install_detailed(&prefix, &shim, PlatformLibraryNames::Linux, 12)
        .expect_err("existing journal must fail");
    assert!(error.to_string().contains("restore it first"));
    std::fs::remove_file(manifest_path(&prefix)).expect("remove journal fixture");

    let destination = lib.join("libopenslide.so.1");
    std::fs::write(&destination, b"original").expect("write original library");
    let backup = backup_path(&destination, 13);
    std::fs::write(&backup, b"collision").expect("write backup collision");
    let error = execute_install_detailed(&prefix, &shim, PlatformLibraryNames::Linux, 13)
        .expect_err("existing backup must fail");
    assert!(error.to_string().contains("backup path already exists"));
    assert_eq!(std::fs::read(&destination).unwrap(), b"original");
    assert_eq!(std::fs::read(&backup).unwrap(), b"collision");
}

#[cfg(unix)]
#[test]
fn install_rejects_a_symlinked_shim_before_staging() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary install directory");
    let target = directory.path().join("real-shim.so");
    let shim = directory.path().join("shim-link.so");
    std::fs::write(&target, b"shim fixture").expect("write shim target");
    symlink(&target, &shim).expect("create shim symlink");

    let error = execute_install_detailed(
        &directory.path().join("prefix"),
        &shim,
        PlatformLibraryNames::Linux,
        14,
    )
    .expect_err("symlinked shim must fail");
    assert!(error
        .to_string()
        .contains("shim library must not be a symlink"));
}

#[test]
fn restore_rolls_back_prior_entries_when_a_later_backup_is_missing() {
    let directory = tempfile::tempdir().expect("temporary restore directory");
    let prefix = directory.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("create library directory");
    let lib = lib.canonicalize().expect("canonical library directory");
    let first = lib.join("libopenslide.so.1");
    let second = lib.join("libopenslide.so");
    let first_backup = backup_path(&first, 20);
    let missing_backup = backup_path(&second, 20);
    std::fs::write(&first, b"installed first").expect("write first installed shim");
    std::fs::write(&second, b"installed second").expect("write second installed shim");
    std::fs::write(&first_backup, b"original first").expect("write first backup");
    let manifest = manifest_path(&prefix);
    std::fs::write(
        &manifest,
        format!(
            "wsi-rs-openslide-shim\t1\tinstalled\n{}\t{}\n{}\t{}\n",
            first.display(),
            first_backup.display(),
            second.display(),
            missing_backup.display()
        ),
    )
    .expect("write restore manifest");

    let error = execute_restore(&prefix, 21).expect_err("missing later backup must fail");
    assert!(
        error.contains("backup is missing"),
        "unexpected error: {error}"
    );
    assert_eq!(std::fs::read(&first).unwrap(), b"installed first");
    assert_eq!(std::fs::read(&first_backup).unwrap(), b"original first");
    assert_eq!(std::fs::read(&second).unwrap(), b"installed second");
    assert!(manifest.exists(), "failed restore must keep its journal");
    assert!(!removed_path(&first, 21).exists());
}

#[test]
fn prepared_restore_keeps_an_uncommitted_original_when_backup_is_absent() {
    let directory = tempfile::tempdir().expect("temporary restore directory");
    let prefix = directory.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("create library directory");
    let lib = lib.canonicalize().expect("canonical library directory");
    let destination = lib.join("libopenslide.so.1");
    let missing_backup = backup_path(&destination, 30);
    std::fs::write(&destination, b"original library").expect("write original library");
    std::fs::write(
        manifest_path(&prefix),
        format!(
            "wsi-rs-openslide-shim\t1\tprepared\n{}\t{}\n",
            destination.display(),
            missing_backup.display()
        ),
    )
    .expect("write prepared manifest");

    execute_restore(&prefix, 31).expect("prepared restore should complete");
    assert_eq!(std::fs::read(&destination).unwrap(), b"original library");
    assert!(!manifest_path(&prefix).exists());
}

#[test]
fn restore_accepts_legacy_major_version_alias_manifests() {
    let directory = tempfile::tempdir().expect("temporary restore directory");
    let prefix = directory.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("create library directory");
    let lib = lib.canonicalize().expect("canonical library directory");
    let destination = lib.join("libopenslide.so.4");
    std::fs::write(&destination, b"legacy installed shim").expect("write legacy destination");
    std::fs::write(
        manifest_path(&prefix),
        format!(
            "wsi-rs-openslide-shim\t1\tinstalled\n{}\t\n",
            destination.display()
        ),
    )
    .expect("write legacy manifest");

    execute_restore(&prefix, 32).expect("legacy manifest should remain restorable");
    assert!(!destination.exists());
    assert!(!manifest_path(&prefix).exists());
}

#[test]
fn restore_manifest_validation_rejects_duplicates_and_bad_backup_names() {
    let directory = tempfile::tempdir().expect("temporary restore directory");
    let prefix = directory.path().join("prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("create library directory");
    let lib = lib.canonicalize().expect("canonical library directory");
    let destination = lib.join("libopenslide.so.1");
    let manifest = manifest_path(&prefix);

    std::fs::write(
        &manifest,
        format!(
            "wsi-rs-openslide-shim\t1\tinstalled\n{}\t\n{}\t\n",
            destination.display(),
            destination.display()
        ),
    )
    .expect("write duplicate manifest");
    let error = execute_restore(&prefix, 40).expect_err("duplicate destination must fail");
    assert!(error.contains("duplicate restore destination"));

    let invalid_backup = lib.join("libopenslide.so.1.wsi_rs-backup-not-a-stamp");
    std::fs::write(
        &manifest,
        format!(
            "wsi-rs-openslide-shim\t1\tinstalled\n{}\t{}\n",
            destination.display(),
            invalid_backup.display()
        ),
    )
    .expect("write invalid backup manifest");
    let error = execute_restore(&prefix, 41).expect_err("invalid backup name must fail");
    assert!(error.contains("invalid restore backup path"));
}

#[test]
fn rollback_removes_unbacked_destinations_stages_and_journal() {
    let directory = tempfile::tempdir().expect("temporary rollback directory");
    let destination = directory.path().join("new-destination.so");
    let stage = directory.path().join("staged.so");
    let manifest = directory.path().join("install.tsv");
    std::fs::write(&destination, b"installed").expect("write installed destination");
    std::fs::write(&stage, b"staged").expect("write stage");
    std::fs::write(&manifest, b"journal").expect("write journal");

    rollback_install(
        &[RestoreEntry {
            destination: destination.clone(),
            backup: None,
        }],
        std::slice::from_ref(&stage),
        &manifest,
    )
    .expect("rollback unbacked destination");

    assert!(!destination.exists());
    assert!(!stage.exists());
    assert!(!manifest.exists());
}

#[test]
fn filesystem_helpers_report_operation_context_and_clean_failed_manifests() {
    let directory = tempfile::tempdir().expect("temporary helper directory");
    let missing = directory.path().join("missing");
    let destination = directory.path().join("destination");

    let error = copy_and_sync(&missing, &destination).expect_err("missing source must fail");
    assert!(error.contains("open shim"), "unexpected error: {error}");

    let source = directory.path().join("source");
    std::fs::write(&source, b"source").expect("write source");
    std::fs::write(&destination, b"occupied").expect("write occupied destination");
    let error = copy_and_sync(&source, &destination).expect_err("occupied stage must fail");
    assert!(
        error.contains("create staged shim"),
        "unexpected error: {error}"
    );

    let manifest = missing.join("install.tsv");
    let error = write_manifest(&manifest, &[], "prepared").expect_err("missing parent must fail");
    assert!(error.contains("create"), "unexpected error: {error}");
    assert!(!manifest.with_extension("tsv.tmp").exists());

    let error = reject_symlink(&missing, "missing fixture").expect_err("missing path must fail");
    assert!(error.contains("inspect missing fixture"));
    let error = sync_directory(&missing).expect_err("missing directory must fail");
    assert!(error.contains("sync directory"));
}

#[test]
fn rollback_and_cleanup_helpers_preserve_journal_failure_context() {
    let directory = tempfile::tempdir().expect("temporary cleanup directory");
    let manifest = directory.path().join("install.tsv");
    std::fs::write(&manifest, b"journal").expect("write rollback journal");
    let error = rollback_install_with(
        &[],
        &[],
        &manifest,
        |_path| Err(std::io::Error::other("injected journal removal failure")),
        |_from, _to| Ok(()),
    )
    .expect_err("journal removal failure must be reported");
    assert!(error.contains("injected journal removal failure"));
    assert!(error.contains(&manifest.display().to_string()));

    let first = directory.path().join("first.stage");
    let second = directory.path().join("second.stage");
    std::fs::write(&first, b"first").expect("write first staged file");
    std::fs::write(&second, b"second").expect("write second staged file");
    cleanup_paths(&[first.clone(), second.clone()]);
    assert!(!first.exists());
    assert!(!second.exists());
}

#[test]
fn manifest_commit_and_validation_report_filesystem_boundaries() {
    let directory = tempfile::tempdir().expect("temporary manifest boundary directory");
    let occupied_manifest = directory.path().join("manifest.tsv");
    std::fs::create_dir(&occupied_manifest).expect("create occupied manifest directory");
    let error = write_manifest(&occupied_manifest, &[], "prepared")
        .expect_err("manifest commit onto a directory must fail");
    assert!(
        error.contains("commit manifest"),
        "unexpected error: {error}"
    );
    assert!(!occupied_manifest.with_extension("tsv.tmp").exists());

    let standalone_manifest = directory.path().join("standalone.tsv");
    std::fs::write(
        &standalone_manifest,
        "wsi-rs-openslide-shim\t1\tinstalled\n",
    )
    .expect("write standalone manifest");
    let missing_prefix = directory.path().join("missing-prefix");
    let error = read_and_validate_manifest(&missing_prefix, &standalone_manifest)
        .expect_err("missing prefix library directory must fail");
    assert!(error.contains("resolve prefix library directory"));
}

#[cfg(unix)]
#[test]
fn copy_and_restore_report_directory_failures() {
    let directory = tempfile::tempdir().expect("temporary filesystem failure directory");
    let staged = directory.path().join("staged.so");
    let error = copy_and_sync(directory.path(), &staged)
        .expect_err("copying a directory as a shim must fail while reading");
    assert!(error.contains("stage"), "unexpected error: {error}");

    let prefix = directory.path().join("directory-prefix");
    let lib = prefix.join("lib");
    std::fs::create_dir_all(&lib).expect("create directory restore prefix");
    let lib = lib.canonicalize().expect("canonical library directory");
    let destination = lib.join("libopenslide.so.1");
    std::fs::create_dir(&destination).expect("create directory-shaped destination");
    std::fs::write(
        manifest_path(&prefix),
        format!(
            "wsi-rs-openslide-shim\t1\tinstalled\n{}\t\n",
            destination.display()
        ),
    )
    .expect("write directory restore manifest");
    let error = execute_restore(&prefix, 70)
        .expect_err("removing a directory-shaped installed shim must fail");
    assert!(error.contains("remove restored shim"));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn library_verification_reports_a_loaded_library_without_the_openslide_symbol() {
    #[cfg(target_os = "macos")]
    let library = Path::new("/usr/lib/libSystem.B.dylib");
    #[cfg(target_os = "linux")]
    let library = Path::new("libc.so.6");

    let error = verify_library_version(library)
        .expect_err("system C library must not expose the OpenSlide version symbol");
    assert!(
        error.contains("load openslide_get_version"),
        "unexpected error: {error}"
    );
}
