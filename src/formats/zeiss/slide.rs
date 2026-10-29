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
        self.slide.read_tile(
            req.scene.get(),
            req.series.get(),
            req.level.get(),
            req.col,
            req.row,
        )
    }

    /// A classic-JPEG subblock that is exactly the tile is returned as
    /// stored; one that covers the tile is cropped losslessly in the
    /// coefficient domain from the MCU corner at or before the tile, or
    /// returned whole with the tile region inside it when the crop does not
    /// fit the lossless-crop limit. A JPEG XR subblock covering the tile is
    /// returned as its whole stream with the tile region inside it. Tiles
    /// painted only from uncompressed subblocks are returned as their exact
    /// uncompressed RGB samples, which is lossless. Other compositions of
    /// lossy subblocks fail with [`WsiError::Unsupported`]; `tile_location`
    /// reports their parts.
    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        self.slide
            .read_raw_tile(req, self.slide.limits.lossless_crop_bytes())
    }

    /// The lossless-crop limit for tiles cropped from one JPEG subblock.
    fn raw_compressed_tile_reservation_hint(&self, req: &TileRequest) -> Result<u64, WsiError> {
        self.slide.raw_tile_reservation_hint(req)
    }

    fn tile_location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        self.slide.tile_location(req)
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
        let tile = crate::formats::geometry::native_regular_display_tile(
            &self.slide.dataset,
            req,
            "Zeiss raw JPEG display",
        )?;
        // Display reads are admitted at one encoded unit, so their crops
        // must fit it.
        let limits = self.slide.limits;
        self.slide.read_raw_tile(
            &tile,
            limits
                .lossless_crop_bytes()
                .min(limits.encoded_unit_bytes()),
        )
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.slide.read_associated(name)
    }
}

pub(super) struct ZeissSlide {
    pub(super) probe: crate::core::test_hooks::SourceProbe,
    pub(super) limits: crate::SlideLimits,
    pub(super) source_path: PathBuf,
    /// `source_path`, shared by every tile location.
    pub(super) location_path: Arc<Path>,
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
    /// Data part of each subblock, in directory order, from the open-time
    /// preflight of every subblock header.
    pub(super) subblock_data: Vec<ByteRange>,
}

impl ZeissSlide {
    pub(super) fn validate_wsi_pixels(&self) -> Result<(), WsiError> {
        let czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
        if czi.subblocks().iter().any(|info| {
            info.pixel_type != CziPixelType::Bgr24 || !info.compression.is_wsi_supported()
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
        let Some(level) = u32::try_from(level_idx).ok().and_then(|level_idx| {
            self.dataset
                .find_level(SceneId::new(0), SeriesId::new(0), LevelIdx::new(level_idx))
        }) else {
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
        self.source_identity.ensure_matches(
            &self.source_path,
            "CZI source identity changed before associated-image read",
        )?;
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
