use std::path::{Component, Path, PathBuf};

use crate::error::WsiError;

/// Resolve an existing metadata-named companion file without allowing the
/// metadata to escape the slide bundle.
pub(crate) fn resolve_companion_file(
    slide_path: &Path,
    root: &Path,
    metadata_path: &str,
) -> Result<PathBuf, WsiError> {
    let relative = Path::new(metadata_path);
    if metadata_path.is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid_companion(slide_path, metadata_path, "unsafe path"));
    }

    let canonical_root = root.canonicalize().map_err(|error| {
        invalid_companion(
            slide_path,
            metadata_path,
            format!("cannot resolve slide root: {error}"),
        )
    })?;
    let candidate = canonical_root.join(relative);
    let canonical_candidate = candidate.canonicalize().map_err(|error| {
        invalid_companion(
            slide_path,
            metadata_path,
            format!("cannot resolve companion file: {error}"),
        )
    })?;
    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(invalid_companion(
            slide_path,
            metadata_path,
            "path resolves outside the slide root",
        ));
    }
    if !canonical_candidate.is_file() {
        return Err(invalid_companion(
            slide_path,
            metadata_path,
            "companion is not a regular file",
        ));
    }
    Ok(canonical_candidate)
}

fn invalid_companion(
    slide_path: &Path,
    metadata_path: &str,
    reason: impl std::fmt::Display,
) -> WsiError {
    WsiError::InvalidSlide {
        path: slide_path.to_path_buf(),
        message: format!("invalid companion path {metadata_path:?}: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_existing_file_below_root() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join("data")).expect("data directory");
        std::fs::write(root.path().join("data/tile.jpg"), b"tile").expect("tile");
        let path =
            resolve_companion_file(&root.path().join("slide.ini"), root.path(), "data/tile.jpg")
                .expect("valid companion");
        assert!(path.ends_with("data/tile.jpg"));
    }

    #[test]
    fn rejects_absolute_and_parent_paths() {
        let root = tempfile::tempdir().expect("root");
        for value in ["../secret", "/tmp/secret"] {
            assert!(
                resolve_companion_file(&root.path().join("slide.ini"), root.path(), value).is_err()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret"), b"secret").expect("secret");
        symlink(outside.path().join("secret"), root.path().join("escape")).expect("symlink");

        assert!(
            resolve_companion_file(&root.path().join("slide.ini"), root.path(), "escape").is_err()
        );
    }
}
