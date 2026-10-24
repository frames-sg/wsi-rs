use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::types::TileRequest;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WsiError {
    #[error("read cancelled")]
    Cancelled,
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("TIFF error in {path}: {message}")]
    Tiff { path: PathBuf, message: String },
    #[error("JPEG decode error: {0}")]
    Jpeg(String),
    #[error("JPEG2000 decode error: {0}")]
    Jp2k(String),
    #[error("XML parse error: {0}")]
    Xml(String),
    #[error("invalid slide {path}: {message}")]
    InvalidSlide { path: PathBuf, message: String },
    #[error("tile read failed at ({col}, {row}) level {level}: {reason}")]
    TileRead {
        col: i64,
        row: i64,
        level: u32,
        reason: String,
    },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("I/O error at {path}: {source}")]
    IoWithPath {
        #[source]
        source: Arc<std::io::Error>,
        path: PathBuf,
    },

    /// An input or requested output exceeded a checked byte budget.
    #[error(
        "resource limit exceeded for {resource}: requested {requested} bytes, limit {limit} bytes"
    )]
    ResourceLimit {
        resource: &'static str,
        requested: u64,
        limit: u64,
    },

    // --- New variants for multi-dimensional engine ---
    #[error("scene index {index} out of range (dataset has {count} scenes)")]
    SceneOutOfRange { index: usize, count: usize },

    #[error("series index {index} out of range (scene has {count} series)")]
    SeriesOutOfRange { index: usize, count: usize },

    #[error("level {level} out of range (series has {count} levels)")]
    LevelOutOfRange { level: u32, count: u32 },

    #[error("plane axis {axis} value {value} exceeds max {max}")]
    PlaneOutOfRange { axis: String, value: u32, max: u32 },

    #[error("associated image not found: {0}")]
    AssociatedImageNotFound(String),

    #[error("display conversion error: {0}")]
    DisplayConversion(String),

    #[error("backend contract violation in {context}: expected {expected} results, got {actual}")]
    BackendContract {
        context: &'static str,
        expected: usize,
        actual: usize,
    },

    /// Codec-layer error from a j2k backend.
    #[error("codec error in {codec}: {source}")]
    Codec {
        codec: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Operation is intentionally unsupported on this path.
    #[error("unsupported: {reason}")]
    Unsupported { reason: String },
}

impl WsiError {
    /// `InvalidSlide` for the file at `path`.
    pub(crate) fn invalid_slide(path: &Path, message: impl Into<String>) -> Self {
        Self::InvalidSlide {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }

    /// `IoWithPath` for an I/O failure on the file at `path`.
    pub(crate) fn io_with_path(path: &Path, source: std::io::Error) -> Self {
        Self::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        }
    }

    /// `TileRead` at the column, row and level that `req` names.
    pub(crate) fn tile_read(req: &TileRequest, reason: impl Into<String>) -> Self {
        Self::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests;
