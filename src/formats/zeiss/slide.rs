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

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        match self.slide.exact_raw_jpeg_subblock(req) {
            Ok(Some(_)) => TileCodecKind::Jpeg,
            Ok(None) | Err(_) => TileCodecKind::Other,
        }
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        read_cpu_tiles(reqs, |req, backend| {
            self.read_tile_with_backend(req, backend)
        })
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Cpu)
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        self.slide.read_raw_jpeg_tile(req)
    }

    fn read_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        let scene =
            self.slide
                .dataset
                .scenes
                .get(req.scene.get())
                .ok_or(WsiError::SceneOutOfRange {
                    index: req.scene.get(),
                    count: self.slide.dataset.scenes.len(),
                })?;
        let series = scene
            .series
            .get(req.series.get())
            .ok_or(WsiError::SeriesOutOfRange {
                index: req.series.get(),
                count: scene.series.len(),
            })?;
        let level =
            series
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: series.levels.len() as u32,
                })?;
        let TileLayout::Regular {
            tile_width,
            tile_height,
            ..
        } = level.tile_layout
        else {
            return Err(WsiError::Unsupported {
                reason: "Zeiss raw JPEG display access requires a regular native tile grid".into(),
            });
        };
        if (req.tile_width, req.tile_height) != (tile_width, tile_height) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "Zeiss raw JPEG display tile size {}x{} does not match native tile size {}x{}",
                    req.tile_width, req.tile_height, tile_width, tile_height
                ),
            });
        }

        self.slide.read_raw_jpeg_tile(&TileRequest {
            scene: req.scene,
            series: req.series,
            level: req.level,
            plane: req.plane,
            col: req.col,
            row: req.row,
        })
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
    #[cfg(test)]
    pub(super) subblock_decodes: AtomicU64,
    pub(super) limits: crate::SlideLimits,
    pub(super) source_path: PathBuf,
    pub(super) source_identity: FileIdentity,
    pub(super) dataset: Dataset,
    pub(super) czi: Mutex<CziFile>,
    pub(super) preflight_file: Mutex<File>,
    pub(super) level_cache: LevelImageCache,
    pub(super) tile_cache: LocalTileCache,
    pub(super) subblock_cache: Mutex<PrivateCache<u64, Arc<CpuTile>>>,
    pub(super) associated_cache: Mutex<PrivateCache<String, Arc<CpuTile>>>,
    pub(super) associated_sources: HashMap<String, czi_rs::AttachmentInfo>,
    pub(super) subblock_origin: (i32, i32),
    pub(super) canvas_level_subblocks: Vec<Vec<usize>>,
    pub(super) canvas_level_tile_subblocks: Vec<CanvasTileSubblockMap>,
}

impl ZeissSlide {
    pub(super) fn validate_wsi_pixels(&self) -> Result<(), WsiError> {
        let czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
        if czi.subblocks().iter().any(|info| {
            info.pixel_type != CziPixelType::Bgr24
                || !matches!(
                    info.compression,
                    CziCompressionMode::UnCompressed
                        | CziCompressionMode::Jpg
                        | CziCompressionMode::JpgXr
                )
        }) {
            return Err(WsiError::UnsupportedFormat("CZI WSI reads currently require Bgr24 with uncompressed, JPEG, or JPEG XR subblocks".into()));
        }
        Ok(())
    }

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
        if FileIdentity::from_path(&self.source_path)? != self.source_identity {
            return Err(WsiError::InvalidSlide {
                path: self.source_path.clone(),
                message: "CZI source identity changed before associated-image read".into(),
            });
        }
        let buffer = {
            let mut czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
            let (_, buffer) = decode_associated_attachment(&mut czi, attachment, self.limits)?
                .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
            buffer
        };
        let arc = Arc::new(buffer);
        let retained_bytes = u64::try_from(arc.data.byte_size()).unwrap_or(u64::MAX);
        self.associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(name.to_string(), arc.clone(), retained_bytes);
        Ok(arc.as_ref().clone())
    }
}
