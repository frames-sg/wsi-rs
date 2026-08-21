use super::*;

// ── Slide ──────────────────────────────────────────────────

/// Top-level handle. Owns the SlideReader + shared cache.
pub struct Slide {
    source: Box<dyn SlideReader>,
    cache: RwLock<Arc<TileCache>>,
    display_cache: Arc<TileCache>,
    max_region_pixels: u64,
    decode_runtime: Arc<DecodeRuntime>,
}

#[derive(Clone, Copy)]
struct TileReadTraceOutcome {
    fallback_to_cpu: bool,
    fallback_reason: &'static str,
    device_decoded_host_resident: bool,
}

impl TileReadTraceOutcome {
    fn classify(result: &Result<TilePixels, WsiError>, device_decode_attempted: bool) -> Self {
        let (fallback_to_cpu, fallback_reason) = match result {
            Ok(TilePixels::Cpu(_)) if device_decode_attempted => (true, "j2k_auto_chose_cpu"),
            Err(WsiError::Unsupported { .. }) if device_decode_attempted => {
                (true, "no_device_backend_for_codec")
            }
            _ => (false, "none"),
        };
        Self {
            fallback_to_cpu,
            fallback_reason,
            device_decoded_host_resident: false,
        }
    }

    fn record(self, span: &tracing::Span) {
        span.record("fallback_to_cpu", self.fallback_to_cpu);
        span.record("fallback_reason", self.fallback_reason);
        span.record(
            "device_decoded_host_resident",
            self.device_decoded_host_resident,
        );
    }
}

impl std::fmt::Debug for Slide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slide")
            .field("dataset_id", &self.source.dataset().id)
            .finish()
    }
}

impl Slide {
    /// Construct from an already-opened source and cache.
    pub(crate) fn from_source(source: Box<dyn SlideReader>, cache: Arc<TileCache>) -> Self {
        let decode_runtime = DecodeRuntime::default_arc();
        Self {
            source: Box::new(AdaptiveDecodeReader::new(source, decode_runtime.clone())),
            cache: RwLock::new(cache),
            display_cache: Arc::new(TileCache::display_default()),
            max_region_pixels: DEFAULT_MAX_REGION_PIXELS,
            decode_runtime,
        }
    }

    pub(crate) fn from_source_with_config_and_runtime(
        source: Box<dyn SlideReader>,
        cache_config: CacheConfig,
        max_region_pixels: u64,
        decode_runtime: Arc<DecodeRuntime>,
    ) -> Self {
        let source_hint = source.recommended_shared_cache_bytes();
        Self {
            source: Box::new(AdaptiveDecodeReader::new(source, decode_runtime.clone())),
            cache: RwLock::new(Arc::new(TileCache::shared_with_config(
                cache_config,
                source_hint,
            ))),
            display_cache: Arc::new(TileCache::display_with_config(cache_config)),
            max_region_pixels,
            decode_runtime,
        }
    }

    /// Construct from an already-opened source with an internal cache budget.
    pub fn from_source_with_cache_bytes(source: Box<dyn SlideReader>, cache_bytes: u64) -> Self {
        Self::from_source(source, Arc::new(TileCache::new(cache_bytes)))
    }

    /// Zero-config entry point: builtin registry + source-aware default cache.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WsiError> {
        Self::open_with_options(path, SlideOpenOptions::default())
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: SlideOpenOptions,
    ) -> Result<Self, WsiError> {
        let resolved_path = crate::formats::svcache::resolve_open_path_with_policy(
            path.as_ref(),
            options.svcache_policy,
        )?;
        let source = options
            .registry
            .open_with_cache_config(&resolved_path, options.cache_config)?;
        let decode_runtime = DecodeRuntime::arc_for_options(options.decode_execution_options)?;
        Ok(Self::from_source_with_config_and_runtime(
            source,
            options.cache_config,
            options.max_region_pixels,
            decode_runtime,
        ))
    }

    /// Open with the given registry and cache.
    ///
    /// Reusing the same [`TileCache`] across multiple handles allows decoded
    /// tiles from one handle to satisfy later reads from another handle that
    /// targets the same dataset and plane.
    pub(crate) fn open_with(
        path: impl AsRef<Path>,
        registry: &FormatRegistry,
        cache: Arc<TileCache>,
    ) -> Result<Self, WsiError> {
        let source = registry.open(path.as_ref())?;
        let mut slide = Self::from_source(source, cache);
        slide.max_region_pixels = DEFAULT_MAX_REGION_PIXELS;
        Ok(slide)
    }

    /// Open with the given registry and an internal cache budget.
    pub fn open_with_cache_bytes(
        path: impl AsRef<Path>,
        registry: &FormatRegistry,
        cache_bytes: u64,
    ) -> Result<Self, WsiError> {
        Self::open_with(path, registry, Arc::new(TileCache::new(cache_bytes)))
    }

    pub fn dataset(&self) -> &Dataset {
        self.source.dataset()
    }

    pub fn decode_execution_options(&self) -> DecodeExecutionOptions {
        self.decode_runtime.options()
    }

    pub fn level_source_kind(
        &self,
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
    ) -> Result<LevelSourceKind, WsiError> {
        self.source
            .level_source_kind(scene.into(), series.into(), level.into())
    }

    /// Prepares format-specific level state without decoding any pixels.
    pub fn prepare_level_controlled(
        &self,
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
        control: &crate::ReadControl,
    ) -> Result<(), WsiError> {
        self.source
            .prepare_level_controlled(scene.into(), series.into(), level.into(), control)
    }

    pub fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.source.tile_codec_kind(req)
    }

    /// Replace the decoded tile cache attached to this slide.
    ///
    /// The slide clones the supplied owner, so callers may release their
    /// [`Arc`] immediately. The detached cache is returned to make replacement
    /// lifetime and accounting explicit.
    pub fn replace_shared_tile_cache(&self, cache: Arc<TileCache>) -> Arc<TileCache> {
        let mut attached = self
            .cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::replace(&mut *attached, cache)
    }

    fn shared_tile_cache(&self) -> Arc<TileCache> {
        self.cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn cached_tile_present(&self, req: &TileRequest) -> bool {
        let key = CacheKey::from_tile_request(self.dataset().id, req);
        self.shared_tile_cache().get(&key).is_some()
    }

    pub fn source(&self) -> &dyn SlideReader {
        self.source.as_ref()
    }

    pub fn read_tile(
        &self,
        req: &TileRequest,
        output: TileOutputPreference,
    ) -> Result<TilePixels, WsiError> {
        let device_decode_attempted = matches!(
            output,
            TileOutputPreference::PreferDevice { .. } | TileOutputPreference::RequireDevice { .. }
        );
        let span = tracing::debug_span!(
            "wsi_read_tile",
            device_decode_attempted,
            fallback_to_cpu = tracing::field::Empty,
            fallback_reason = tracing::field::Empty,
            device_decoded_host_resident = tracing::field::Empty,
        );
        let _guard = span.enter();
        let result = self.source.read_tile(req, output);
        let outcome = TileReadTraceOutcome::classify(&result, device_decode_attempted);
        outcome.record(&span);
        tracing::debug!(
            device_decode_attempted,
            fallback_to_cpu = outcome.fallback_to_cpu,
            fallback_reason = outcome.fallback_reason,
            device_decoded_host_resident = outcome.device_decoded_host_resident,
            "wsi tile output preference resolved"
        );
        result
    }

    pub fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.source.read_tiles(reqs, output)
    }

    /// Reads tiles with cooperative cancellation delegated to the source.
    ///
    /// Existing batch APIs remain unchanged. This controlled path preserves
    /// the source batch while checking cancellation around its admission.
    pub fn read_tiles_controlled(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
        control: &crate::ReadControl,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.source.read_tiles_controlled(reqs, output, control)
    }

    /// Reads one tile with cooperative cancellation checks around source work.
    pub fn read_tile_controlled(
        &self,
        req: &TileRequest,
        output: TileOutputPreference,
        control: &crate::ReadControl,
    ) -> Result<TilePixels, WsiError> {
        let tiles = self.read_tiles_controlled(std::slice::from_ref(req), output, control)?;
        crate::core::batch::exactly_one(tiles, "controlled single tile read")
    }

    pub fn read_raw_compressed_tile(
        &self,
        req: &TileRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        self.source.read_raw_compressed_tile(req)
    }

    pub fn read_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        self.source.read_raw_compressed_display_tile(req)
    }

    /// Read a pixel region, compositing from cached or freshly-decoded tiles.
    ///
    /// Validates all indices (scene, series, level, plane axes) before reading.
    /// Output buffer metadata (color_space, channels, sample_type, layout) is
    /// inherited from the first decoded tile -- no hardcoded assumptions.
    ///
    /// Only `CpuTileLayout::Interleaved` is supported for compositing. Planar
    /// tiles return `WsiError::DisplayConversion`.
    pub fn read_region(&self, req: &RegionRequest) -> Result<CpuTile, WsiError> {
        check_region_pixel_limit(req.size_px.0, req.size_px.1, self.max_region_pixels)?;
        let cache = self.shared_tile_cache();
        let mut ctx = SlideReadContext::new(
            Some(cache.as_ref()),
            TileOutputPreference::cpu(),
            self.max_region_pixels,
        );
        if let Some(result) = self.source.read_region_fastpath(&mut ctx, req) {
            return result;
        }
        composite_region_from_source(
            self.source.as_ref(),
            Some(cache.as_ref()),
            req,
            self.max_region_pixels,
        )
    }

    /// Read a region whose origin includes a fractional level-pixel offset.
    ///
    /// This is the compatibility boundary for APIs such as OpenSlide that
    /// express reduced-level reads in level-0 coordinates. `offset_px` must be
    /// finite and lie in `[0, 1)` on both axes; it is added to
    /// [`RegionRequest::origin_px`] before tile composition. A nonzero offset
    /// returns RGBA8 so the interpolation coverage remains available to
    /// callers that require premultiplied output.
    pub fn read_region_subpixel(
        &self,
        req: &RegionRequest,
        offset_px: (f64, f64),
    ) -> Result<CpuTile, WsiError> {
        if !valid_subpixel_offset(offset_px.0) || !valid_subpixel_offset(offset_px.1) {
            return Err(WsiError::DisplayConversion(format!(
                "subpixel offset must be finite and in [0, 1), got ({}, {})",
                offset_px.0, offset_px.1
            )));
        }
        if offset_px == (0.0, 0.0) {
            return self.read_region(req);
        }

        let cache = self.shared_tile_cache();
        composite_fractional_region_from_source(
            self.source.as_ref(),
            Some(cache.as_ref()),
            req,
            (
                req.origin_px.0 as f64 + offset_px.0,
                req.origin_px.1 as f64 + offset_px.1,
            ),
            self.max_region_pixels,
        )
    }

    pub fn read_display_tile(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        self.read_display_tile_impl(req, TileOutputPreference::cpu())
    }

    pub fn read_display_tile_with_output(
        &self,
        req: &TileViewRequest,
        output: TileOutputPreference,
    ) -> Result<CpuTile, WsiError> {
        self.read_display_tile_impl(req, output)
    }

    fn read_display_tile_impl(
        &self,
        req: &TileViewRequest,
        output: TileOutputPreference,
    ) -> Result<CpuTile, WsiError> {
        // For Regular tile layouts, route through the generic composition path
        // with cache so intermediate tile reads are reused. For WholeLevel and
        // Irregular layouts, delegate to the source's override which may have
        // format-specific fast paths (e.g. NDPI MCU-level JPEG access).
        let is_regular = self
            .source
            .dataset()
            .scenes
            .get(req.scene.get())
            .and_then(|s| s.series.get(req.series.get()))
            .and_then(|s| s.levels.get(req.level.get() as usize))
            .is_some_and(|level| matches!(level.tile_layout, TileLayout::Regular { .. }));
        if is_regular {
            let display_cache = self
                .source
                .use_display_tile_cache(req)
                .then_some(self.display_cache.as_ref());
            read_display_tile_from_source(self.source.as_ref(), display_cache, req, output)
        } else if matches!(output, TileOutputPreference::RequireDevice { .. }) {
            Err(WsiError::Unsupported {
                reason: "this format-specific display tile path requires CPU output".into(),
            })
        } else {
            self.source.read_display_tile(req)
        }
    }

    /// Convenience: read a region and convert to RgbaImage.
    /// Only works for Uint8 data (brightfield). For Uint16/Float32,
    /// use read_region() + to_rgba_windowed() with an explicit DisplayWindow.
    pub fn read_region_rgba(&self, req: &RegionRequest) -> Result<image::RgbaImage, WsiError> {
        self.read_region(req)?.to_rgba()
    }

    /// Read a region and convert to RgbaImage with explicit windowing.
    /// For Uint16/Float32 data (fluorescence, computed images).
    pub fn read_region_rgba_windowed(
        &self,
        req: &RegionRequest,
        window: &DisplayWindow,
    ) -> Result<image::RgbaImage, WsiError> {
        self.read_region(req)?.to_rgba_windowed(window)
    }

    /// Read an associated image (label, macro, thumbnail).
    /// Direct delegation to the underlying SlideReader. No caching.
    pub fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.source.read_associated(name)
    }
}

fn valid_subpixel_offset(value: f64) -> bool {
    value.is_finite() && (0.0..1.0).contains(&value)
}

#[cfg(test)]
mod tests;
