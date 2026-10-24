use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::error::WsiError;

/// Filesystem identity used to decide whether parsed slide data can be reused:
/// short-lived probe-to-open caches, and MIRAX indexes shared across handles
/// for as long as any handle is open.
///
/// Including metadata prevents a parsed object from being reused after the
/// path has been replaced or modified. A rewrite in place that keeps the same
/// length and inode within one modification-time tick is not detected, so on
/// filesystems with coarse timestamps such a rewrite can reuse stale data.
/// Probe-to-open entries are consumed on open, so directory identities do not
/// need to fingerprint every child.
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
            std::fs::canonicalize(path).map_err(|source| WsiError::io_with_path(path, source))?;
        let metadata = std::fs::metadata(&canonical_path)
            .map_err(|source| WsiError::io_with_path(&canonical_path, source))?;
        Ok(Self::from_metadata(canonical_path, metadata))
    }

    pub(crate) fn from_open_file(path: &Path, file: &File) -> Result<Self, WsiError> {
        let canonical_path =
            std::fs::canonicalize(path).map_err(|source| WsiError::io_with_path(path, source))?;
        let metadata = file
            .metadata()
            .map_err(|source| WsiError::io_with_path(&canonical_path, source))?;
        Ok(Self::from_metadata(canonical_path, metadata))
    }

    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    /// Modification time in nanoseconds since the Unix epoch, when the
    /// filesystem reports one.
    pub(crate) fn modified_ns(&self) -> Option<u128> {
        self.modified_ns
    }

    /// Opens `path` and fails with `InvalidSlide` carrying `message` unless
    /// the opened file is `expected`. The identity comes from the open file,
    /// so the path cannot be swapped between the check and the open.
    pub(crate) fn open_verified(
        path: &Path,
        expected: &Self,
        message: &str,
    ) -> Result<File, WsiError> {
        let file = File::open(path).map_err(|source| WsiError::io_with_path(path, source))?;
        expected.ensure_same(&Self::from_open_file(path, &file)?, path, message)?;
        Ok(file)
    }

    /// Fails with `InvalidSlide` carrying `message` unless the file now at
    /// `path` is this one.
    pub(crate) fn ensure_matches(&self, path: &Path, message: &str) -> Result<(), WsiError> {
        self.ensure_same(&Self::from_path(path)?, path, message)
    }

    /// Fails with `InvalidSlide` carrying `message` unless `actual` is this
    /// identity.
    pub(crate) fn ensure_same(
        &self,
        actual: &Self,
        path: &Path,
        message: &str,
    ) -> Result<(), WsiError> {
        if actual == self {
            return Ok(());
        }
        Err(WsiError::invalid_slide(path, message))
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
