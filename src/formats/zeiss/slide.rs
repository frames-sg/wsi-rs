use super::attachments::decode_associated_attachment;
use super::metadata::CanvasTileSubblockMap;
use super::*;

mod parse;

type LevelImageCache = Mutex<PrivateCache<(usize, usize), Arc<CpuTile>>>;

/// Subblock headers read to estimate one level's compression ratio.
const ENCODING_SAMPLE_SUBBLOCKS: usize = 64;
type LocalTileCache = Mutex<PrivateCache<(usize, usize, i64, i64), CpuTile>>;

pub(super) struct ZeissReader {
    pub(super) slide: Arc<ZeissSlide>,
}

impl SlideReader for ZeissReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn tile_is_present(&self, req: &TileRequest) -> Result<Option<bool>, WsiError> {
        if req.scene.get() != 0
            || req.series.get() != 0
            || req.plane.get() != PlaneSelection::default()
        {
            return Ok(None);
        }
        let tiles = self
            .slide
            .canvas_level_tile_subblocks
            .get(req.level.get() as usize)
            .ok_or(WsiError::LevelOutOfRange {
                level: req.level.get(),
                count: self.slide.canvas_level_tile_subblocks.len() as u32,
            })?;
        Ok(Some(
            tiles
                .get(&(req.col, req.row))
                .is_some_and(|indices| !indices.is_empty()),
        ))
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        match self.slide.exact_raw_jpeg_subblock(req) {
            Ok(Some(_)) => TileCodecKind::Jpeg,
            Ok(None) | Err(_) => TileCodecKind::Other,
        }
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.read_cpu_batch(reqs)
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Cpu)
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        self.slide.read_raw_jpeg_tile(req)
    }

    fn level_encoding(
        &self,
        _scene: SceneId,
        _series: SeriesId,
        level: LevelIdx,
    ) -> Result<Option<SourceEncoding>, WsiError> {
        Ok(usize::try_from(level.get())
            .ok()
            .and_then(|level| self.slide.level_encoding(level)))
    }

    fn associated_encoding(&self, name: &str) -> Result<Option<SourceEncoding>, WsiError> {
        Ok(self.slide.associated_encoding(name))
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
    pub(super) probe: crate::core::test_hooks::SourceProbe,
    pub(super) limits: crate::SlideLimits,
    pub(super) source_path: PathBuf,
    pub(super) source_identity: FileIdentity,
    pub(super) dataset: Dataset,
    pub(super) czi: Mutex<CziContainer>,
    pub(super) preflight_file: Mutex<File>,
    pub(super) level_cache: LevelImageCache,
    pub(super) tile_cache: LocalTileCache,
    pub(super) subblock_cache: Mutex<PrivateCache<u64, Arc<CpuTile>>>,
    pub(super) source_flights: crate::core::cache::TileFlights<u64>,
    pub(super) associated_cache: Mutex<PrivateCache<String, Arc<CpuTile>>>,
    pub(super) associated_sources: HashMap<String, AttachmentInfo>,
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

    /// One codec and an estimated compression ratio for a level, measured on
    /// at most [`ENCODING_SAMPLE_SUBBLOCKS`] evenly spaced subblocks.
    pub(super) fn level_encoding(&self, level: usize) -> Option<SourceEncoding> {
        if self.dataset.scenes.len() == 1 && self.level_tiles_are_raw_jpeg(level) {
            // Every tile is served raw, so callers measure exact per-tile sizes.
            return None;
        }
        let indices = self.canvas_level_subblocks.get(level)?;
        let mut czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
        let codec = czi.subblocks().get(*indices.first()?)?.compression;
        if indices
            .iter()
            .any(|&index| czi.subblocks().get(index).map(|info| info.compression) != Some(codec))
        {
            return None;
        }
        let step = indices.len().div_ceil(ENCODING_SAMPLE_SUBBLOCKS).max(1);
        let (mut decoded, mut encoded) = (0_u64, 0_u64);
        for &index in indices.iter().step_by(step) {
            let info = czi.subblocks().get(index)?;
            let pixels =
                u64::from(info.stored_size.w).checked_mul(u64::from(info.stored_size.h))?;
            decoded = decoded.checked_add(
                pixels.checked_mul(u64::try_from(info.pixel_type.bytes_per_pixel()).ok()?)?,
            )?;
            encoded = encoded.checked_add(czi.subblock_data_size(index).ok()?)?;
        }
        Some(SourceEncoding::new(codec.compression()).with_sizes(decoded, encoded))
    }

    fn level_tiles_are_raw_jpeg(&self, level_idx: usize) -> bool {
        let Some(tiles) = self
            .canvas_level_tile_subblocks
            .get(level_idx)
            .filter(|tiles| !tiles.is_empty())
        else {
            return false;
        };
        let Some(level) = self
            .dataset
            .scenes
            .first()
            .and_then(|scene| scene.series.first())
            .and_then(|series| series.levels.get(level_idx))
        else {
            return false;
        };
        let czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
        tiles
            .iter()
            .all(|(&(col, row), indices)| match indices.as_slice() {
                [index] => czi
                    .subblocks()
                    .get(*index)
                    .is_some_and(|info| self.subblock_is_exact_tile(info, level, col, row)),
                _ => false,
            })
    }

    /// Codec and sizes of an associated attachment; its stored size includes
    /// any embedded-CZI container overhead, so the ratio is a lower bound.
    pub(super) fn associated_encoding(&self, name: &str) -> Option<SourceEncoding> {
        let attachment = self.associated_sources.get(name)?;
        let decoded = self.dataset.associated_images.get(name)?.decoded_bytes()?;
        let compression = if attachment.content_file_type.eq_ignore_ascii_case("JPG") {
            Compression::Jpeg
        } else if attachment.content_file_type.eq_ignore_ascii_case("CZI") {
            let mut czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
            super::attachments::embedded_czi_compression(&mut czi, attachment, self.limits)?
        } else {
            return None;
        };
        Some(SourceEncoding::new(compression).with_sizes(decoded, attachment.data_size))
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
