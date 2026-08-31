use super::*;
use j2k_core::BackendRequest;

// ── Probe traits ───────────────────────────────────────────────────

/// Detects whether a file is a given format. Fast, no full parse.
pub trait FormatProbe: Send + Sync {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError>;
}

/// Internal probe extension for formats whose validation constructs the slide.
///
/// It keeps probing and opening on the same caller-supplied cache policy so a
/// successful probe can be reused without first constructing default caches.
pub(crate) trait ConfiguredFormatProbe: FormatProbe {
    fn probe_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<ProbeResult, WsiError> {
        let _ = config;
        self.probe(path)
    }
}

/// Result from a cheap file-format probe.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProbeResult {
    pub detected: bool,
    pub vendor: String,
    pub confidence: ProbeConfidence,
}

impl ProbeResult {
    /// Creates a positive probe result for a detected vendor.
    pub fn detected(vendor: impl Into<String>, confidence: ProbeConfidence) -> Self {
        Self {
            detected: true,
            vendor: vendor.into(),
            confidence,
        }
    }

    /// Creates a negative probe result for a vendor that did not match.
    ///
    /// The registry ignores `confidence` when `detected` is false.
    pub fn not_detected(vendor: impl Into<String>) -> Self {
        Self {
            detected: false,
            vendor: vendor.into(),
            confidence: ProbeConfidence::Likely,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProbeConfidence {
    Definite,
    Likely,
}

/// Opens a file and returns a SlideReader.
pub trait DatasetReader: Send + Sync {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError>;
}

pub(crate) trait ConfiguredDatasetReader: DatasetReader {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError>;
}

// ── Read interface ─────────────────────────────────────────────────

pub(crate) fn read_cpu_tiles(
    reqs: &[TileRequest],
    mut read_tile: impl FnMut(&TileRequest, BackendRequest) -> Result<CpuTile, WsiError>,
) -> Result<Vec<CpuTile>, WsiError> {
    reqs.iter()
        .map(|req| read_tile(req, BackendRequest::Cpu))
        .collect()
}

pub struct SlideReadContext<'a> {
    tile_cache: Option<&'a TileCache>,
    max_region_pixels: u64,
}

impl<'a> SlideReadContext<'a> {
    pub(crate) fn new(tile_cache: Option<&'a TileCache>, max_region_pixels: u64) -> Self {
        Self {
            tile_cache,
            max_region_pixels,
        }
    }

    pub(crate) fn tile_cache(&self) -> Option<&'a TileCache> {
        self.tile_cache
    }

    pub fn max_region_pixels(&self) -> u64 {
        self.max_region_pixels
    }
}

/// Backend interface used by [`Slide`].
///
/// ```
/// use wsi_rs::{
///     ColorSpace, CpuTile, Dataset, SlideReader, TileRequest, WsiError,
/// };
/// # fn _example() {
/// struct Mock;
/// impl SlideReader for Mock {
///     fn dataset(&self) -> &Dataset { unimplemented!() }
///     fn read_tile_cpu(&self, _: &TileRequest) -> Result<CpuTile, WsiError> {
///         Ok(CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![255, 0, 0]).unwrap())
///     }
/// }
/// let m = Mock;
/// let _ = m.read_tile_cpu(&TileRequest::new(0usize, 0usize, 0, 0, 0));
/// # }
/// ```
pub trait SlideReader: Send + Sync {
    fn dataset(&self) -> &Dataset;
    fn tile_codec_kind(&self, _req: &TileRequest) -> TileCodecKind {
        TileCodecKind::Other
    }
    fn level_source_kind(
        &self,
        scene: SceneId,
        series: SeriesId,
        level: LevelIdx,
    ) -> Result<LevelSourceKind, WsiError> {
        let dataset = self.dataset();
        let scene_ref = dataset
            .scenes
            .get(scene.get())
            .ok_or(WsiError::SceneOutOfRange {
                index: scene.get(),
                count: dataset.scenes.len(),
            })?;
        let series_ref = scene_ref
            .series
            .get(series.get())
            .ok_or(WsiError::SeriesOutOfRange {
                index: series.get(),
                count: scene_ref.series.len(),
            })?;
        if level.get() as usize >= series_ref.levels.len() {
            return Err(WsiError::LevelOutOfRange {
                level: level.get(),
                count: series_ref.levels.len() as u32,
            });
        }
        Ok(LevelSourceKind::Physical)
    }
    /// Prepares format-specific state for a level without decoding pixels.
    ///
    /// The default implementation validates the requested level and otherwise
    /// performs no work, preserving compatibility for existing readers.
    fn prepare_level_controlled(
        &self,
        scene: SceneId,
        series: SeriesId,
        level: LevelIdx,
        control: &crate::ReadControl,
    ) -> Result<(), WsiError> {
        control.check_cancelled()?;
        self.level_source_kind(scene, series, level)?;
        control.check_cancelled()
    }
    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError>;

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        reqs.iter().map(|req| self.read_tile_cpu(req)).collect()
    }

    fn read_tiles_cpu_controlled(
        &self,
        reqs: &[TileRequest],
        control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        control.check_cancelled()?;
        let result = self.read_tiles_cpu(reqs).and_then(|tiles| {
            crate::core::batch::expect_exact_count(tiles, reqs.len(), "controlled tile batch")
        });
        control.check_cancelled()?;
        result
    }

    #[cfg(feature = "metal")]
    fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        _session: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        if reqs.is_empty() {
            return Ok(Vec::new());
        }
        Err(WsiError::Unsupported {
            reason: "Metal-resident JP2K output is not supported by this reader".into(),
        })
    }

    #[cfg(feature = "cuda")]
    fn read_tiles_cuda(
        &self,
        reqs: &[TileRequest],
        _session: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        if reqs.is_empty() {
            return Ok(Vec::new());
        }
        Err(WsiError::Unsupported {
            reason: "CUDA-resident JP2K output is not supported by this reader".into(),
        })
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        Err(WsiError::Unsupported {
            reason: format!(
                "raw compressed tile access is not available for tile ({}, {}) at level {}",
                req.col,
                req.row,
                req.level.get()
            ),
        })
    }
    fn read_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        Err(WsiError::Unsupported {
            reason: format!(
                "raw compressed display tile access is not available for tile ({}, {}) at level {}",
                req.col,
                req.row,
                req.level.get()
            ),
        })
    }
    fn use_display_tile_cache(&self, _req: &TileViewRequest) -> bool {
        true
    }
    fn read_region_fastpath(
        &self,
        _ctx: &mut SlideReadContext<'_>,
        _req: &RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        None
    }
    fn read_region(&self, req: &RegionRequest) -> Result<CpuTile, WsiError> {
        composite_region_from_source(self, None, req, DEFAULT_MAX_REGION_PIXELS)
    }
    fn read_display_tile(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        read_display_tile_from_source(self, None, req)
    }
    fn associated_image(&self, name: &str) -> Result<Option<CpuTile>, WsiError> {
        match self.read_associated(name) {
            Ok(tile) => Ok(Some(tile)),
            Err(WsiError::AssociatedImageNotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }
    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        Err(WsiError::AssociatedImageNotFound(name.into()))
    }
}

/// Internal resource-accounting boundary around public and built-in readers.
///
/// Public registry readers cannot provide parse/decode planning metadata, so
/// they are wrapped by [`ConservativeManagedReader`] and admitted using the
/// configured encoded-unit ceiling.
pub(crate) trait ManagedSlideReader: SlideReader {
    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError>;
    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError>;
    fn display_tile_encoded_upper_bound(&self, req: &TileViewRequest) -> Result<u64, WsiError>;
    fn associated_encoded_upper_bound(&self, name: &str) -> Result<u64, WsiError>;
    fn region_fastpath_encoded_upper_bound(&self, req: &RegionRequest) -> Result<u64, WsiError>;
}

pub(crate) struct ConservativeManagedReader {
    inner: Box<dyn SlideReader>,
    encoded_unit_bytes: u64,
}

impl ConservativeManagedReader {
    pub(crate) fn new(inner: Box<dyn SlideReader>, encoded_unit_bytes: u64) -> Self {
        Self {
            inner,
            encoded_unit_bytes,
        }
    }
}

impl SlideReader for ConservativeManagedReader {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.inner.tile_codec_kind(req)
    }

    fn level_source_kind(
        &self,
        scene: SceneId,
        series: SeriesId,
        level: LevelIdx,
    ) -> Result<LevelSourceKind, WsiError> {
        self.inner.level_source_kind(scene, series, level)
    }

    fn prepare_level_controlled(
        &self,
        scene: SceneId,
        series: SeriesId,
        level: LevelIdx,
        control: &crate::ReadControl,
    ) -> Result<(), WsiError> {
        self.inner
            .prepare_level_controlled(scene, series, level, control)
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_tile_cpu(req)
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.inner.read_tiles_cpu(reqs)
    }

    fn read_tiles_cpu_controlled(
        &self,
        reqs: &[TileRequest],
        control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.inner.read_tiles_cpu_controlled(reqs, control)
    }

    #[cfg(feature = "metal")]
    fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        self.inner.read_tiles_metal(reqs, session)
    }

    #[cfg(feature = "cuda")]
    fn read_tiles_cuda(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        self.inner.read_tiles_cuda(reqs, session)
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        self.inner.read_raw_compressed_tile(req)
    }

    fn read_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        self.inner.read_raw_compressed_display_tile(req)
    }

    fn use_display_tile_cache(&self, req: &TileViewRequest) -> bool {
        self.inner.use_display_tile_cache(req)
    }

    fn read_region_fastpath(
        &self,
        ctx: &mut SlideReadContext<'_>,
        req: &RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        self.inner.read_region_fastpath(ctx, req)
    }

    fn read_region(&self, req: &RegionRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_region(req)
    }

    fn read_display_tile(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_display_tile(req)
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.inner.read_associated(name)
    }
}

impl ManagedSlideReader for ConservativeManagedReader {
    fn tile_encoded_upper_bound(&self, _req: &TileRequest) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }

    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        Ok(if !reqs.is_empty() {
            self.encoded_unit_bytes
        } else {
            0
        })
    }

    fn display_tile_encoded_upper_bound(&self, _req: &TileViewRequest) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }

    fn associated_encoded_upper_bound(&self, _name: &str) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }

    fn region_fastpath_encoded_upper_bound(&self, _req: &RegionRequest) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }
}
