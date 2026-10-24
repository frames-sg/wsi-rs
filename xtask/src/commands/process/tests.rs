use std::fs;

use super::*;

#[test]
fn clean_worktree_check_distinguishes_clean_dirty_and_non_repository_paths() {
    let repository = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository.path())
        .status()
        .unwrap();
    assert!(status.success());

    assert!(ensure_clean_worktree_at(repository.path()).is_ok());

    fs::write(repository.path().join("untracked.txt"), "dirty").unwrap();
    let error = ensure_clean_worktree_at(repository.path()).unwrap_err();
    assert!(error.contains("working tree must be clean"));
    assert!(error.contains("untracked.txt"));

    let non_repository = tempfile::tempdir().unwrap();
    assert!(ensure_clean_worktree_at(non_repository.path())
        .unwrap_err()
        .contains("git status --porcelain"));
    assert!(
        ensure_clean_worktree_at(&non_repository.path().join("missing"))
            .unwrap_err()
            .contains("failed to start")
    );
}

#[test]
fn program_execution_reports_success_exit_failure_and_spawn_failure() {
    let cargo = cargo();

    assert!(run_program(cargo.clone(), &["--version"], &[("XTASK_CHILD", "1")]).is_ok());
    assert!(run_program(cargo, &["--definitely-invalid"], &[])
        .unwrap_err()
        .contains("exited with"));
    assert!(run_program(
        OsString::from("xtask-command-that-does-not-exist"),
        &[],
        &[]
    )
    .unwrap_err()
    .contains("failed to start"));
}

#[test]
fn captured_program_execution_preserves_output_and_failure_context() {
    let test_binary = std::env::current_exe().unwrap().into_os_string();

    let output = run_program_capture(
        test_binary.clone(),
        &["--list"],
        &[("XTASK_CHILD", "capture")],
    )
    .unwrap();
    assert!(output.contains("commands::process::tests"));

    let error = run_program_capture(test_binary, &["--definitely-invalid"], &[]).unwrap_err();
    assert!(error.contains("exited with"));
    assert!(!error.trim().is_empty());

    assert!(run_program_capture(
        OsString::from("xtask-capture-command-that-does-not-exist"),
        &[],
        &[]
    )
    .unwrap_err()
    .contains("failed to start"));
}

#[test]
fn lenient_capture_records_output_or_the_reason_it_is_unavailable() {
    let version = run_capture_lenient("rustc", &["--version"]);
    assert!(version.starts_with("rustc "), "{version}");
    assert_eq!(version, version.trim());

    let failed = run_capture_lenient("rustc", &["--definitely-invalid"]);
    assert!(failed.starts_with("unavailable: `rustc --definitely-invalid` exited with"));
    let missing = run_capture_lenient("xtask-lenient-command-that-does-not-exist", &[]);
    assert!(
        missing.starts_with("unavailable: failed to start"),
        "{missing}"
    );
}

#[test]
fn git_helpers_run_in_the_workspace_and_report_tree_state() {
    let toplevel = git(&["rev-parse", "--show-toplevel"]).unwrap();
    assert_eq!(
        Path::new(toplevel.trim()).canonicalize().unwrap(),
        workspace_root().canonicalize().unwrap()
    );
    assert_eq!(
        git_lenient(&["rev-parse", "--show-toplevel"]),
        toplevel.trim()
    );
    assert!(git_lenient(&["definitely-not-a-git-command"])
        .starts_with("unavailable: `git definitely-not-a-git-command` exited with"));
    assert!(git_dirty().is_ok());

    let repository = tempfile::tempdir().unwrap();
    git_in(repository.path(), &["init", "--quiet"]).unwrap();
    assert_eq!(git_dirty_in(repository.path()), Ok(false));
    fs::write(repository.path().join("untracked.txt"), "dirty").unwrap();
    assert_eq!(git_dirty_in(repository.path()), Ok(true));

    let error = git_dirty_in(tempfile::tempdir().unwrap().path()).unwrap_err();
    assert!(error.contains("`git status --porcelain"), "{error}");
}

#[test]
fn cargo_wrappers_execute_the_configured_cargo_binary() {
    assert!(run_cargo(&["--version"]).is_ok());
    assert!(run_cargo_with_env(&["--version"], &[("XTASK_CHILD", "cargo")]).is_ok());
    assert!(!cargo().is_empty());
}
