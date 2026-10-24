use super::*;

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
        self.dataset().level(scene, series, level)?;
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
    /// Whether a source tile contains acquired pixels, without decoding it.
    /// `None` means the format cannot establish presence from its metadata.
    /// Pixel color, including black, is never evidence that a tile is absent.
    fn tile_is_present(&self, _req: &TileRequest) -> Result<Option<bool>, WsiError> {
        Ok(None)
    }

    /// How the named associated image is stored, when the format reports it.
    fn associated_encoding(&self, _name: &str) -> Result<Option<SourceEncoding>, WsiError> {
        Ok(None)
    }
    /// How one level's pixels are stored, when the format reports a single
    /// codec for the level without exposing raw tiles.
    fn level_encoding(
        &self,
        _scene: SceneId,
        _series: SeriesId,
        _level: LevelIdx,
    ) -> Result<Option<SourceEncoding>, WsiError> {
        Ok(None)
    }
}

/// Internal resource-accounting boundary around public and built-in readers.
///
/// Public registry readers cannot provide parse/decode planning metadata, so
/// they are wrapped by [`ConservativeManagedReader`] and admitted using the
/// configured encoded-unit ceiling.
pub(crate) trait ManagedSlideReader: SlideReader {
    /// Complete already decoded output without entering a decoder worker. A
    /// declined fast path retains ordinary source execution and admission.
    fn read_tiles_cpu_fastpath(
        &self,
        _reqs: &[TileRequest],
        _control: Option<&crate::ReadControl>,
    ) -> Option<Result<Vec<CpuTile>, WsiError>> {
        None
    }

    fn read_tiles_with_context(
        &self,
        reqs: &[TileRequest],
        context: &crate::core::limits::ReadExecutionContext<'_>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        match context.control {
            Some(control) => self.read_tiles_cpu_controlled(reqs, control),
            None => self.read_tiles_cpu(reqs),
        }
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn prepare_adaptive_jp2k(
        &self,
        _reqs: &[TileRequest],
        _workers: usize,
        _control: Option<&crate::ReadControl>,
    ) -> Option<Result<crate::decode::jp2k::PreparedJp2kBatch, WsiError>> {
        None
    }

    #[cfg(feature = "metal")]
    fn read_metal_with_context(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::metal::MetalBackendSessions,
        context: &crate::core::limits::ReadExecutionContext<'_>,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        // Native owned preparation temporarily coexists with format-owned input.
        let extra = context.try_extra(self.tile_batch_encoded_upper_bound(reqs)?)?;
        if reqs.len() <= 1 || extra.is_some() {
            self.read_tiles_metal(reqs, session)
        } else {
            reqs.iter()
                .map(|req| {
                    crate::core::batch::exactly_one(
                        self.read_tiles_metal(std::slice::from_ref(req), session)?,
                        "bounded strict Metal single tile",
                    )
                })
                .collect()
        }
    }

    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError>;
    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError>;
    fn display_tile_encoded_upper_bound(&self, req: &TileViewRequest) -> Result<u64, WsiError>;
    fn associated_encoded_upper_bound(&self, name: &str) -> Result<u64, WsiError>;
    fn region_fastpath_encoded_upper_bound(&self, req: &RegionRequest) -> Result<u64, WsiError>;
}

/// Expands to the [`SlideReader`] methods a wrapper forwards unchanged to
/// `self.$inner`. Wrapping readers invoke it inside their `impl SlideReader`
/// and write only the methods whose behaviour differs, so a new pure
/// forwarder is added here once rather than to every wrapper.
macro_rules! forward_slide_reader_methods {
    ($inner:ident) => {
        fn dataset(&self) -> &$crate::core::types::Dataset {
            self.$inner.dataset()
        }

        fn tile_codec_kind(
            &self,
            req: &$crate::core::types::TileRequest,
        ) -> $crate::core::types::TileCodecKind {
            self.$inner.tile_codec_kind(req)
        }

        fn level_source_kind(
            &self,
            scene: $crate::core::types::SceneId,
            series: $crate::core::types::SeriesId,
            level: $crate::core::types::LevelIdx,
        ) -> Result<$crate::core::types::LevelSourceKind, $crate::error::WsiError> {
            self.$inner.level_source_kind(scene, series, level)
        }

        fn prepare_level_controlled(
            &self,
            scene: $crate::core::types::SceneId,
            series: $crate::core::types::SeriesId,
            level: $crate::core::types::LevelIdx,
            control: &$crate::ReadControl,
        ) -> Result<(), $crate::error::WsiError> {
            self.$inner
                .prepare_level_controlled(scene, series, level, control)
        }

        #[cfg(feature = "metal")]
        fn read_tiles_metal(
            &self,
            reqs: &[$crate::core::types::TileRequest],
            session: &$crate::output::metal::MetalBackendSessions,
        ) -> Result<Vec<$crate::output::metal::MetalDeviceTile>, $crate::error::WsiError> {
            self.$inner.read_tiles_metal(reqs, session)
        }

        #[cfg(feature = "cuda")]
        fn read_tiles_cuda(
            &self,
            reqs: &[$crate::core::types::TileRequest],
            session: &$crate::output::cuda::CudaBackendSessions,
        ) -> Result<Vec<$crate::output::cuda::CudaDeviceTile>, $crate::error::WsiError> {
            self.$inner.read_tiles_cuda(reqs, session)
        }

        fn read_raw_compressed_tile(
            &self,
            req: &$crate::core::types::TileRequest,
        ) -> Result<$crate::core::types::RawCompressedTile, $crate::error::WsiError> {
            self.$inner.read_raw_compressed_tile(req)
        }

        fn read_raw_compressed_display_tile(
            &self,
            req: &$crate::core::types::TileViewRequest,
        ) -> Result<$crate::core::types::RawCompressedTile, $crate::error::WsiError> {
            self.$inner.read_raw_compressed_display_tile(req)
        }

        fn use_display_tile_cache(&self, req: &$crate::core::types::TileViewRequest) -> bool {
            self.$inner.use_display_tile_cache(req)
        }

        fn read_associated(
            &self,
            name: &str,
        ) -> Result<$crate::core::types::CpuTile, $crate::error::WsiError> {
            self.$inner.read_associated(name)
        }

        fn associated_encoding(
            &self,
            name: &str,
        ) -> Result<Option<$crate::core::types::SourceEncoding>, $crate::error::WsiError> {
            self.$inner.associated_encoding(name)
        }

        fn tile_is_present(
            &self,
            req: &$crate::core::types::TileRequest,
        ) -> Result<Option<bool>, $crate::error::WsiError> {
            self.$inner.tile_is_present(req)
        }

        fn level_encoding(
            &self,
            scene: $crate::core::types::SceneId,
            series: $crate::core::types::SeriesId,
            level: $crate::core::types::LevelIdx,
        ) -> Result<Option<$crate::core::types::SourceEncoding>, $crate::error::WsiError> {
            self.$inner.level_encoding(scene, series, level)
        }
    };
}
pub(crate) use forward_slide_reader_methods;

pub(crate) struct ConservativeManagedReader {
    inner: Box<dyn SlideReader>,
    encoded_unit_bytes: u64,
    region_fastpath_in_pool: bool,
}

impl ConservativeManagedReader {
    /// Wraps a custom reader, whose region fast path runs inside the process
    /// decode pool as its only worker context.
    pub(crate) fn new(inner: Box<dyn SlideReader>, encoded_unit_bytes: u64) -> Self {
        Self {
            inner,
            encoded_unit_bytes,
            region_fastpath_in_pool: true,
        }
    }

    /// Wraps a built-in reader. Built-ins enter the pool only for actual decode
    /// work, so a declined or cached fast path never pays a pool round trip.
    pub(crate) fn builtin(inner: Box<dyn SlideReader>, encoded_unit_bytes: u64) -> Self {
        Self {
            region_fastpath_in_pool: false,
            ..Self::new(inner, encoded_unit_bytes)
        }
    }
}

impl SlideReader for ConservativeManagedReader {
    forward_slide_reader_methods!(inner);

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

    fn read_region_fastpath(
        &self,
        ctx: &mut SlideReadContext<'_>,
        req: &RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        // Preserve the worker context of custom readers; built-ins decide
        // whether their fast path needs CPU work before entering the pool.
        if !self.region_fastpath_in_pool {
            return self.inner.read_region_fastpath(ctx, req);
        }
        crate::core::decode_runtime::DecodeRuntime::default_arc()
            .install_codec_cpu(|| self.inner.read_region_fastpath(ctx, req))
    }

    fn read_region(&self, req: &RegionRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_region(req)
    }

    fn read_display_tile(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_display_tile(req)
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
