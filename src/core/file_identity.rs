use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use crate::error::WsiError;

/// Filesystem identity used for short-lived probe-to-open caches.
///
/// Including metadata prevents a parsed object from being reused after the
/// path has been replaced or modified. Cache entries are still consumed on
/// open, so directory identities do not need to fingerprint every child.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FileIdentity {
    canonical_path: PathBuf,
    length: u64,
    modified_ns: Option<u128>,
    is_dir: bool,
}

impl FileIdentity {
    pub(crate) fn from_path(path: &Path) -> Result<Self, WsiError> {
        let canonical_path =
            std::fs::canonicalize(path).map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
        let metadata =
            std::fs::metadata(&canonical_path).map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: canonical_path.clone(),
            })?;
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());

        Ok(Self {
            canonical_path,
            length: metadata.len(),
            modified_ns,
            is_dir: metadata.is_dir(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_changes_when_file_length_changes() {
        let temp = tempfile::tempdir().expect("temp directory");
        let path = temp.path().join("slide.bin");
        std::fs::write(&path, b"one").expect("write initial file");
        let first = FileIdentity::from_path(&path).expect("first identity");

        std::fs::write(&path, b"replacement").expect("replace file");
        let second = FileIdentity::from_path(&path).expect("second identity");

        assert_ne!(first, second);
    }

    #[test]
    fn missing_path_is_an_explicit_error() {
        let temp = tempfile::tempdir().expect("temp directory");
        let error = FileIdentity::from_path(&temp.path().join("missing"))
            .expect_err("missing path must fail");
        assert!(matches!(error, WsiError::IoWithPath { .. }));
    }
}
