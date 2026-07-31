use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(super) fn crate_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub(super) fn tracked_text_files(root: &Path) -> Vec<PathBuf> {
    let output = Command::new("git")
        .args([
            "-C",
            root.to_str().expect("UTF-8 crate root"),
            "ls-files",
            "-z",
        ])
        .output()
        .expect("run git ls-files");
    assert!(output.status.success(), "git ls-files must succeed");
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| root.join(String::from_utf8_lossy(path).as_ref()))
        // `git ls-files` reports index entries that are deleted in a dirty
        // release-candidate tree. Those paths are not part of the candidate.
        .filter(|path| path.is_file())
        .filter(|path| is_text_file(path))
        .collect()
}

fn is_text_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if matches!(name, ".gitignore" | "LICENSE") {
        return true;
    }
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("rs" | "md" | "toml" | "yml" | "yaml" | "sh" | "py" | "txt" | "lock" | "example")
    )
}

pub(super) fn relative_path(path: &Path) -> String {
    path.strip_prefix(crate_root())
        .unwrap_or(path)
        .display()
        .to_string()
}
