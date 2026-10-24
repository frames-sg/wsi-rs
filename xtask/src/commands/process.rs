use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The workspace this xtask was built from.
pub(super) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest has a workspace parent")
        .to_path_buf()
}

pub(super) fn ensure_clean_worktree() -> Result<(), String> {
    ensure_clean_worktree_at(Path::new("."))
}

fn ensure_clean_worktree_at(path: &Path) -> Result<(), String> {
    let status = git_in(path, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        Ok(())
    } else {
        Err(format!(
            "working tree must be clean before packaging:\n{status}"
        ))
    }
}

pub(super) fn run_cargo(args: &[&str]) -> Result<(), String> {
    run_cargo_with_env(args, &[])
}

pub(super) fn run_cargo_with_env(args: &[&str], envs: &[(&str, &str)]) -> Result<(), String> {
    run_program(cargo(), args, envs)
}

pub(super) fn run_program(
    program: OsString,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(), String> {
    let display = program.to_string_lossy();
    eprintln!("+ {} {}", display, args.join(" "));
    let mut command = Command::new(&program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    run_status(&mut command)
}

/// Run `program` without echoing it first. Its arguments may be paths that
/// are not UTF-8.
pub(super) fn run_quiet(program: impl AsRef<OsStr>, args: &[OsString]) -> Result<(), String> {
    run_status(Command::new(program).args(args))
}

fn run_status(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|err| format!("failed to start `{program}`: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program}` exited with {status}"))
    }
}

pub(super) fn run_program_capture(
    program: OsString,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<String, String> {
    let display = program.to_string_lossy();
    eprintln!("+ {} {}", display, args.join(" "));
    let mut command = Command::new(&program);
    command.args(args);
    // Capture API text on stdout while keeping build progress visible in CI.
    command.stderr(Stdio::inherit());
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|err| format!("failed to start `{}`: {err}", display))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "`{}` exited with {}; see command diagnostics above",
            display, output.status
        ))
    }
}

/// The stdout of `program`, run quietly. A failure reports the command and
/// its stderr.
pub(super) fn capture<I, S>(program: impl AsRef<OsStr>, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    capture_output(Command::new(program).args(args))
}

/// The trimmed stdout of `program`, or why it was unavailable. Capture
/// metadata records the reason rather than failing the capture.
pub(super) fn run_capture_lenient(program: &str, args: &[&str]) -> String {
    lenient(capture(program, args))
}

/// [`git`]'s trimmed stdout, or why it was unavailable, as
/// [`run_capture_lenient`] reports it.
pub(super) fn git_lenient(args: &[&str]) -> String {
    lenient(git(args))
}

fn lenient(stdout: Result<String, String>) -> String {
    stdout.map_or_else(
        |error| format!("unavailable: {error}"),
        |stdout| stdout.trim().to_string(),
    )
}

/// The stdout of `git args` run in the workspace root.
pub(super) fn git(args: &[&str]) -> Result<String, String> {
    git_in(&workspace_root(), args)
}

/// The stdout of `git args` run in `directory`.
pub(super) fn git_in(directory: &Path, args: &[&str]) -> Result<String, String> {
    capture_output(Command::new("git").current_dir(directory).args(args))
}

/// Whether the workspace has uncommitted changes, counting untracked files.
pub(super) fn git_dirty() -> Result<bool, String> {
    git_dirty_in(&workspace_root())
}

fn git_dirty_in(directory: &Path) -> Result<bool, String> {
    git_in(
        directory,
        &["status", "--porcelain", "--untracked-files=normal"],
    )
    .map(|status| !status.trim().is_empty())
}

fn capture_output(command: &mut Command) -> Result<String, String> {
    let display = command_display(command);
    let output = command
        .output()
        .map_err(|err| format!("failed to start `{display}`: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "`{display}` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn command_display(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn cargo() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

#[cfg(test)]
mod tests;
