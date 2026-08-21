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
fn cargo_wrappers_execute_the_configured_cargo_binary() {
    assert!(run_cargo(&["--version"]).is_ok());
    assert!(run_cargo_with_env(&["--version"], &[("XTASK_CHILD", "cargo")]).is_ok());
    assert!(!cargo().is_empty());
}
