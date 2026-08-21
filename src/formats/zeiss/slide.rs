use super::attachments::decode_associated_attachment;
use super::metadata::CanvasTileSubblockMap;
use super::*;

mod parse;

type LevelImageCache = Mutex<PrivateCache<(usize, usize), Arc<CpuTile>>>;
type LocalTileCache = Mutex<PrivateCache<(usize, usize, i64, i64), Arc<CpuTile>>>;

#[cfg(test)]
pub(super) static ZEISS_LOCAL_TILE_HITS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
pub(super) static ZEISS_DIRECT_LEVEL_COMPOSE_HITS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
pub(super) static ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS: AtomicU64 = AtomicU64::new(0);

pub(super) struct ZeissReader {
    pub(super) slide: Arc<ZeissSlide>,
}

impl SlideReader for ZeissReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        read_cpu_tiles_with_backend(
            reqs,
            output,
            "RequireDevice is not supported for Zeiss",
            |req, backend| self.read_tile_with_backend(req, backend),
        )
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Auto)
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.slide.read_associated(name)
    }
}

impl ZeissReader {
    fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        self.slide.read_tile(
            req.scene.get(),
            req.series.get(),
            req.level.get(),
            req.col,
            req.row,
            backend,
        )
    }
}

pub(super) struct ZeissSlide {
    pub(super) source_path: PathBuf,
    pub(super) dataset: Dataset,
    pub(super) czi: Mutex<CziFile>,
    pub(super) level_cache: LevelImageCache,
    pub(super) tile_cache: LocalTileCache,
    pub(super) associated_cache: Mutex<PrivateCache<String, Arc<CpuTile>>>,
    pub(super) associated_sources: HashMap<String, czi_rs::AttachmentInfo>,
    pub(super) subblock_origin: (i32, i32),
    pub(super) canvas_level_subblocks: Vec<Vec<usize>>,
    pub(super) canvas_level_tile_subblocks: Vec<CanvasTileSubblockMap>,
}

impl ZeissSlide {
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        Self::parse_with_cache_config(path, CacheConfig::deterministic())
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        if let Some(cached) = self
            .associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
        {
            return Ok(cached.as_ref().clone());
        }

        let attachment = self
            .associated_sources
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        let buffer = {
            let mut czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
            let (_, buffer) = decode_associated_attachment(&mut czi, attachment)?
                .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
            buffer
        };
        let arc = Arc::new(buffer);
        self.associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(name.to_string(), arc.clone());
        Ok(arc.as_ref().clone())
    }
}
