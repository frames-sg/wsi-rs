use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

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
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
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
        Ok(Self::from_metadata(canonical_path, metadata))
    }

    pub(crate) fn from_open_file(path: &Path, file: &File) -> Result<Self, WsiError> {
        let canonical_path =
            std::fs::canonicalize(path).map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
        let metadata = file.metadata().map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: canonical_path.clone(),
        })?;
        Ok(Self::from_metadata(canonical_path, metadata))
    }

    fn from_metadata(canonical_path: PathBuf, metadata: std::fs::Metadata) -> Self {
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());

        Self {
            canonical_path,
            length: metadata.len(),
            modified_ns,
            is_dir: metadata.is_dir(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

#[cfg(test)]
mod tests;
