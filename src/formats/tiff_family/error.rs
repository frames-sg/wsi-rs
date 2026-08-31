use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::WsiError;

/// Unique identity for an IFD, derived from its byte offset in the file.
/// Defined here (not in container.rs) to avoid circular dependency with the error type.
#[derive(Clone, Copy, Hash, Eq, PartialEq, Debug)]
pub(crate) struct IfdId(pub u64);

impl fmt::Display for IfdId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IFD@{}", self.0)
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub(crate) enum TiffParseError {
    #[error("I/O error ({kind}): {source}")]
    Io {
        kind: std::io::ErrorKind,
        #[source]
        source: Arc<std::io::Error>,
        path: Option<Arc<PathBuf>>,
    },

    #[error("invalid tag in IFD at offset {ifd_offset}, tag {tag}: {message}")]
    InvalidTag {
        ifd_offset: u64,
        tag: u16,
        message: String,
    },

    #[error("out of bounds: offset {offset}, len {len}")]
    Bounds { offset: u64, len: u64 },

    #[error("invalid TIFF structure: {0}")]
    Structure(String),

    #[error("IFD not found: {0}")]
    IfdNotFound(IfdId),

    #[error("{resource} requires {requested} bytes, exceeding the {limit}-byte limit")]
    ResourceLimit {
        resource: &'static str,
        requested: u64,
        limit: u64,
    },

    #[error("tag not found: IFD at offset {ifd_offset}, tag {tag}")]
    TagNotFound { ifd_offset: u64, tag: u16 },
}

impl From<std::io::Error> for TiffParseError {
    fn from(e: std::io::Error) -> Self {
        let kind = e.kind();
        TiffParseError::Io {
            kind,
            source: Arc::new(e),
            path: None,
        }
    }
}

impl TiffParseError {
    /// Convert to WsiError at the module boundary. Requires the file path
    /// because TiffParseError does not always carry one.
    pub(crate) fn into_wsi_error(self, path: &Path) -> WsiError {
        match self {
            TiffParseError::Io {
                source,
                path: io_path,
                ..
            } => WsiError::IoWithPath {
                source,
                path: io_path
                    .map(|p| p.as_ref().clone())
                    .unwrap_or_else(|| path.to_path_buf()),
            },
            TiffParseError::ResourceLimit {
                resource,
                requested,
                limit,
            } => WsiError::ResourceLimit {
                resource,
                requested,
                limit,
            },
            other => WsiError::Tiff {
                path: path.to_path_buf(),
                message: other.to_string(),
            },
        }
    }
}

#[cfg(test)]
#[path = "error/tests.rs"]
mod tests;
