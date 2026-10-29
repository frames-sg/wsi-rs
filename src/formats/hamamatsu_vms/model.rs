use super::*;

pub(super) struct VmsSlide {
    pub(super) dataset: Dataset,
    pub(super) levels: Vec<VmsLevel>,
    pub(super) associated_files: HashMap<String, VmsAssociatedFile>,
    pub(super) associated_cache: Mutex<PrivateCache<String, Arc<CpuTile>>>,
    pub(super) encoded_unit_bytes: u64,
}

/// A standalone associated JPEG, reread on each uncached read.
pub(super) struct VmsAssociatedFile {
    pub(super) path: PathBuf,
    /// Identity when the slide was opened; a file that no longer matches is
    /// refused.
    pub(super) identity: FileIdentity,
}

pub(super) struct VmsLevel {
    pub(super) scale_denom: u32,
    pub(super) jpegs: Vec<Arc<VmsJpeg>>,
    pub(super) jpegs_across: u32,
    pub(super) base_tiles_across: u32,
    pub(super) base_tiles_down: u32,
}

pub(super) struct VmsJpeg {
    pub(super) path: PathBuf,
    /// `path`, shared by every tile location in this file.
    pub(super) location_path: Arc<Path>,
    /// Decoder parameters shared by every tile of this file.
    pub(super) codec: EncodedCodec,
    /// Standalone header of each tile shape (interior, right edge, bottom
    /// edge, corner): the file header with SOF set to the tile size and the
    /// restart interval removed. Built on first use and shared by tiles.
    pub(super) tile_prefixes: [OnceLock<Arc<[u8]>>; 4],
    /// Sequential cursor used only while extending the lazy restart-marker index.
    pub(super) file: Mutex<File>,
    /// Positional payload reads stay independent so warm-index readers can run concurrently.
    pub(super) payload_file: crate::core::positioned_file::PositionedFile,
    pub(super) header: Vec<u8>,
    pub(super) sof_dimensions_offset: usize,
    pub(super) file_len: u64,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
    pub(super) tiles_across: u32,
    pub(super) tiles_down: u32,
    pub(super) mcu_starts: Mutex<Vec<Option<u64>>>,
    pub(super) unreliable_mcu_starts: Vec<Option<u64>>,
    pub(super) decoded_tile_cache: Mutex<PrivateCache<(usize, u32), Arc<CpuTile>>>,
    pub(super) comment: Option<String>,
    pub(super) encoded_unit_bytes: u64,
}

impl VmsLevel {
    pub(super) fn new(
        path: &Path,
        jpegs: Vec<Arc<VmsJpeg>>,
        jpegs_across: u32,
        _jpegs_down: u32,
        scale_denom: u32,
    ) -> Result<Self, WsiError> {
        let first = jpegs
            .first()
            .ok_or_else(|| WsiError::invalid_slide(path, "VMS level has no JPEG shards"))?
            .clone();
        Ok(Self {
            scale_denom,
            jpegs,
            jpegs_across,
            base_tiles_across: first.tiles_across,
            base_tiles_down: first.tiles_down,
        })
    }
}
