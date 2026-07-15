use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub(super) fn crate_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub(super) fn read_repo_text(relative: &str) -> String {
    let path = crate_root().join(relative);
    if path.is_dir() {
        let mut files = Vec::new();
        collect_text_files(&path, &mut files);
        files.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"));
        files.sort();
        return files
            .into_iter()
            .map(|path| {
                let text = fs::read_to_string(&path).unwrap_or_else(|err| {
                    panic!("read {}: {err}", path.display());
                });
                normalize_line_endings(text)
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    let text =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    normalize_line_endings(text)
}

fn normalize_line_endings(text: String) -> String {
    text.replace("\r\n", "\n")
}

#[test]
fn repository_text_normalizes_windows_line_endings() {
    assert_eq!(
        normalize_line_endings("alpha\r\nbeta\r\n".to_owned()),
        "alpha\nbeta\n"
    );
}

pub(super) fn markdown_link_targets(markdown: &str) -> Vec<&str> {
    markdown
        .split("](")
        .skip(1)
        .filter_map(|tail| tail.split_once(')').map(|(target, _)| target))
        .collect()
}

pub(super) fn path_matches_package_exclude(path: &str, exclude: &str) -> bool {
    if let Some(prefix) = exclude.strip_suffix("/**") {
        path.starts_with(&format!("{prefix}/"))
    } else {
        path == exclude
    }
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

fn collect_text_files(path: &Path, files: &mut Vec<PathBuf>) {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if matches!(name, ".git" | "target") {
        return;
    }

    let metadata = fs::metadata(path).unwrap_or_else(|err| {
        panic!("stat {}: {err}", path.display());
    });
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .unwrap_or_else(|err| panic!("read dir {}: {err}", path.display()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|err| panic!("read dir entry under {}: {err}", path.display()));
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_text_files(&entry.path(), files);
        }
        return;
    }

    if is_text_file(path) {
        files.push(path.to_path_buf());
    }
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

pub(super) fn assert_non_exhaustive_enum(relative: &str, enum_name: &str) {
    let source = read_repo_text(relative);
    let needle = format!("pub enum {enum_name}");
    let Some(enum_start) = source.find(&needle) else {
        panic!("{relative} must define public enum `{enum_name}`");
    };
    let preceding = &source[..enum_start];
    let has_attribute = preceding
        .lines()
        .rev()
        .take_while(|line| {
            let trimmed = line.trim();
            trimmed.is_empty()
                || trimmed.starts_with("#[")
                || trimmed.starts_with("///")
                || trimmed.starts_with("//")
        })
        .any(|line| line.trim() == "#[non_exhaustive]");
    assert!(
        has_attribute,
        "{relative} public enum `{enum_name}` must be #[non_exhaustive] before 1.0"
    );
}

pub(super) fn assert_non_exhaustive_struct(relative: &str, struct_name: &str) {
    let source = read_repo_text(relative);
    let needle = format!("pub struct {struct_name}");
    let Some(struct_start) = source.find(&needle) else {
        panic!("{relative} must define public struct `{struct_name}`");
    };
    let preceding = &source[..struct_start];
    let has_attribute = preceding
        .lines()
        .rev()
        .take_while(|line| {
            let trimmed = line.trim();
            trimmed.is_empty()
                || trimmed.starts_with("#[")
                || trimmed.starts_with("///")
                || trimmed.starts_with("//")
        })
        .any(|line| line.trim() == "#[non_exhaustive]");
    assert!(
        has_attribute,
        "{relative} public struct `{struct_name}` must be #[non_exhaustive] before 1.0"
    );
}

pub(super) fn relative_path(path: &Path) -> String {
    path.strip_prefix(crate_root())
        .unwrap_or(path)
        .display()
        .to_string()
}
