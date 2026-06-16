use super::*;

pub(super) struct VmsSlide {
    pub(super) dataset: Dataset,
    pub(super) levels: Vec<VmsLevel>,
    pub(super) associated_paths: HashMap<String, PathBuf>,
    pub(super) associated_cache: Mutex<LruCache<String, Arc<CpuTile>>>,
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
    pub(super) file: Mutex<File>,
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
    pub(super) decoded_tile_cache: Mutex<LruCache<(usize, u32), Arc<CpuTile>>>,
    pub(super) comment: Option<String>,
}

impl VmsLevel {
    pub(super) fn new(
        jpegs: Vec<Arc<VmsJpeg>>,
        jpegs_across: u32,
        _jpegs_down: u32,
        scale_denom: u32,
    ) -> Result<Self, WsiError> {
        let first = jpegs
            .first()
            .ok_or_else(|| WsiError::InvalidSlide {
                path: PathBuf::new(),
                message: "VMS level has no JPEG shards".into(),
            })?
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

pub(super) fn invalid_slide(path: &Path, message: impl Into<String>) -> WsiError {
    WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

pub(super) fn dataset_id_from_quickhash(
    path: &Path,
    quickhash: &str,
) -> Result<DatasetId, WsiError> {
    if quickhash.len() < 32 {
        return Err(invalid_slide(path, "quickhash too short"));
    }
    let value = u128::from_str_radix(&quickhash[..32], 16)
        .map_err(|_| invalid_slide(path, "quickhash is not valid hex"))?;
    Ok(DatasetId::new(value))
}
