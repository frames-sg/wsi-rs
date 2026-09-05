use super::*;

// ── Slide ──────────────────────────────────────────────────

/// Top-level handle. Owns the SlideReader + shared cache.
pub struct Slide {
    source: Box<dyn ManagedSlideReader>,
    cache: RwLock<Arc<TileCache>>,
    display_cache: Arc<TileCache>,
    limits: SlideLimits,
    admission: Arc<SlideAdmission>,
    decode_runtime: Arc<DecodeRuntime>,
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
        let limits = SlideLimits::default();
        let managed: Box<dyn ManagedSlideReader> = Box::new(ConservativeManagedReader::new(
            source,
            limits.encoded_unit_bytes(),
        ));
        Self {
            source: Box::new(AdaptiveDecodeReader::new_managed(
                managed,
                decode_runtime.clone(),
            )),
            cache: RwLock::new(cache),
            display_cache: Arc::new(TileCache::display_default()),
            limits,
            admission: SlideAdmission::new(limits.slide_transient_bytes()),
            decode_runtime,
        }
    }

    #[cfg(test)]
    #[cfg(test)]
    pub(crate) fn from_source_with_config_and_runtime(
        source: Box<dyn SlideReader>,
        cache_config: CacheConfig,
        limits: SlideLimits,
        decode_runtime: Arc<DecodeRuntime>,
    ) -> Self {
        let managed: Box<dyn ManagedSlideReader> = Box::new(ConservativeManagedReader::new(
            source,
            limits.encoded_unit_bytes(),
        ));
        Self::from_managed_source_with_config_and_runtime(
            managed,
            cache_config,
            limits,
            decode_runtime,
        )
    }

    fn from_managed_source_with_config_and_runtime(
        source: Box<dyn ManagedSlideReader>,
        cache_config: CacheConfig,
        limits: SlideLimits,
        decode_runtime: Arc<DecodeRuntime>,
    ) -> Self {
        Self {
            source: Box::new(AdaptiveDecodeReader::new_managed(
                source,
                decode_runtime.clone(),
            )),
            cache: RwLock::new(Arc::new(TileCache::shared_with_config(cache_config))),
            display_cache: Arc::new(TileCache::display_with_config(cache_config)),
            limits,
            admission: SlideAdmission::new(limits.slide_transient_bytes()),
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
        let source = options.registry.open_with_config(
            &resolved_path,
            BackendOpenConfig::new(options.cache_config, options.limits),
        )?;
        validate_dataset_limits(source.dataset(), options.limits)?;
        let decode_runtime = DecodeRuntime::arc_for_options(options.decode_execution_options)?;
        Ok(Self::from_managed_source_with_config_and_runtime(
            source,
            options.cache_config,
            options.limits,
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
        validate_dataset_limits(source.dataset(), SlideLimits::default())?;
        let mut slide = Self::from_source(source, cache);
        slide.limits = SlideLimits::default();
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

    pub fn limits(&self) -> SlideLimits {
        self.limits
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

    pub fn read_tile(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        let output_bytes = self.estimate_tile_output_bytes(req)?;
        let transient =
            self.ordinary_work_bytes(self.source.tile_encoded_upper_bound(req)?, output_bytes)?;
        let _reservation = self.admission.reserve(transient, None)?;
        let tile = self.source.read_tile_cpu(req)?;
        self.validate_decoded_output(&tile, "decoded tile")?;
        Ok(tile)
    }

    pub fn read_tiles(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_admitted(reqs, None)
    }

    /// Reads tiles with cooperative cancellation delegated to the source.
    ///
    /// Existing batch APIs remain unchanged. This controlled path preserves
    /// the source batch while checking cancellation around its admission.
    pub fn read_tiles_controlled(
        &self,
        reqs: &[TileRequest],
        control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_admitted(reqs, Some(control))
    }

    /// Reads one tile with cooperative cancellation checks around source work.
    pub fn read_tile_controlled(
        &self,
        req: &TileRequest,
        control: &crate::ReadControl,
    ) -> Result<CpuTile, WsiError> {
        let tiles = self.read_tiles_controlled(std::slice::from_ref(req), control)?;
        crate::core::batch::exactly_one(tiles, "controlled single tile read")
    }

    #[cfg(feature = "metal")]
    pub fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        self.read_tiles_device_admitted(reqs, "admitted Metal tile batch", |chunk| {
            self.source.read_tiles_metal(chunk, session)
        })
    }

    #[cfg(feature = "metal")]
    pub fn read_tile_metal(
        &self,
        req: &TileRequest,
        session: &crate::output::metal::MetalBackendSessions,
    ) -> Result<crate::output::metal::MetalDeviceTile, WsiError> {
        crate::core::batch::exactly_one(
            self.read_tiles_metal(std::slice::from_ref(req), session)?,
            "single Metal tile read",
        )
    }

    #[cfg(feature = "cuda")]
    pub fn read_tiles_cuda(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        self.read_tiles_device_admitted(reqs, "admitted CUDA tile batch", |chunk| {
            self.source.read_tiles_cuda(chunk, session)
        })
    }

    #[cfg(feature = "cuda")]
    pub fn read_tile_cuda(
        &self,
        req: &TileRequest,
        session: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<crate::output::cuda::CudaDeviceTile, WsiError> {
        crate::core::batch::exactly_one(
            self.read_tiles_cuda(std::slice::from_ref(req), session)?,
            "single CUDA tile read",
        )
    }

    pub fn read_raw_compressed_tile(
        &self,
        req: &TileRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        let promised = self.source.tile_encoded_upper_bound(req)?;
        let work = ReadWork::new(promised, 0)
            .encoded_only_bytes(self.limits.operation_transient_bytes())?;
        let _reservation = self.admission.reserve(work, None)?;
        let tile = self.source.read_raw_compressed_tile(req)?;
        self.validate_encoded_contract(tile.data().len(), promised, "raw compressed tile")?;
        Ok(tile)
    }

    pub fn read_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        let promised = self.source.display_tile_encoded_upper_bound(req)?;
        let work = ReadWork::new(promised, 0)
            .encoded_only_bytes(self.limits.operation_transient_bytes())?;
        let _reservation = self.admission.reserve(work, None)?;
        let tile = self.source.read_raw_compressed_display_tile(req)?;
        self.validate_encoded_contract(tile.data().len(), promised, "raw compressed display tile")?;
        Ok(tile)
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
        let output_bytes = self.check_region_output(req)?;
        let encoded = self.source.region_fastpath_encoded_upper_bound(req)?;
        let (source_bytes, largest_source_bytes) = self.region_source_work(req, None)?;
        let _reservation = self.admission.reserve(
            self.region_work_bytes(encoded, output_bytes, largest_source_bytes, false)?,
            None,
        )?;
        check_region_pixel_limit(req.size_px.0, req.size_px.1, self.limits.region_pixels())?;
        let cache = self.shared_tile_cache();
        let mut ctx = SlideReadContext::new(Some(cache.as_ref()), self.limits.region_pixels());
        if let Some(result) = self.source.read_region_fastpath(&mut ctx, req) {
            let tile = result?;
            self.validate_decoded_output(&tile, "decoded region")?;
            return Ok(tile);
        }
        let tile = if source_bytes > output_bytes {
            composite_region_from_source_streaming(
                self.source.as_ref(),
                Some(cache.as_ref()),
                req,
                self.limits.region_pixels(),
            )?
        } else {
            composite_region_from_source(
                self.source.as_ref(),
                Some(cache.as_ref()),
                req,
                self.limits.region_pixels(),
            )?
        };
        self.validate_decoded_output(&tile, "decoded region")?;
        Ok(tile)
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

        let output_bytes = self.check_region_output(req)?;
        let encoded = self.source.region_fastpath_encoded_upper_bound(req)?;
        let origin = (
            req.origin_px.0 as f64 + offset_px.0,
            req.origin_px.1 as f64 + offset_px.1,
        );
        let (source_bytes, largest_source_bytes) = self.region_source_work(req, Some(origin))?;
        let _reservation = self.admission.reserve(
            self.region_work_bytes(encoded, output_bytes, largest_source_bytes, true)?,
            None,
        )?;
        let cache = self.shared_tile_cache();
        let tile = if source_bytes > output_bytes {
            composite_fractional_region_from_source_streaming(
                self.source.as_ref(),
                Some(cache.as_ref()),
                req,
                origin,
                self.limits.region_pixels(),
            )?
        } else {
            composite_fractional_region_from_source(
                self.source.as_ref(),
                Some(cache.as_ref()),
                req,
                origin,
                self.limits.region_pixels(),
            )?
        };
        self.validate_decoded_output(&tile, "decoded region")?;
        Ok(tile)
    }

    pub fn read_display_tile(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        self.read_display_tile_impl(req)
    }

    fn read_display_tile_impl(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        let output_bytes = checked_rgba_bytes(req.tile_width, req.tile_height, "display tile")?;
        self.check_output_limit(output_bytes, "decoded tile/associated output")?;
        let encoded = self.source.display_tile_encoded_upper_bound(req)?;
        let _reservation = self
            .admission
            .reserve(self.ordinary_work_bytes(encoded, output_bytes)?, None)?;
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
            let tile = read_display_tile_from_source(self.source.as_ref(), display_cache, req)?;
            self.validate_decoded_output(&tile, "decoded display tile")?;
            Ok(tile)
        } else {
            let tile = self.source.read_display_tile(req)?;
            self.validate_decoded_output(&tile, "decoded display tile")?;
            Ok(tile)
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

    fn read_tiles_admitted(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_admitted_with(
            reqs,
            control,
            "admitted CPU tile batch",
            |chunk| {
                if let Some(control) = control {
                    self.source.read_tiles_cpu_controlled(chunk, control)
                } else {
                    self.source.read_tiles_cpu(chunk)
                }
            },
            |tile| self.validate_decoded_output(tile, "decoded tile"),
        )
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn read_tiles_device_admitted<T>(
        &self,
        reqs: &[TileRequest],
        context: &'static str,
        decode: impl FnMut(&[TileRequest]) -> Result<Vec<T>, WsiError>,
    ) -> Result<Vec<T>, WsiError> {
        self.read_tiles_admitted_with(reqs, None, context, decode, |_| Ok(()))
    }

    fn read_tiles_admitted_with<T>(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
        context: &'static str,
        mut decode: impl FnMut(&[TileRequest]) -> Result<Vec<T>, WsiError>,
        mut validate: impl FnMut(&T) -> Result<(), WsiError>,
    ) -> Result<Vec<T>, WsiError> {
        let estimates = reqs
            .iter()
            .map(|req| self.estimate_tile_output_bytes(req))
            .collect::<Result<Vec<_>, _>>()?;
        let target = self
            .limits
            .batch_chunk_bytes()
            .min(self.limits.operation_transient_bytes())
            .min(self.limits.slide_transient_bytes());
        let mut output = Vec::with_capacity(reqs.len());
        let mut start = 0;
        while start < reqs.len() {
            if let Some(control) = control {
                control.check_cancelled()?;
            }
            let mut end = start;
            let mut chunk_output_bytes = 0_u64;
            let mut chunk_bytes = 0_u64;
            while end < reqs.len() {
                let next_output_bytes = estimates[end];
                let candidate_encoded_bytes = self
                    .source
                    .tile_batch_encoded_upper_bound(&reqs[start..=end])?;
                let candidate_output_bytes = chunk_output_bytes.saturating_add(next_output_bytes);
                let candidate_bytes =
                    ReadWork::new(candidate_encoded_bytes, candidate_output_bytes)
                        .ordinary_bytes(u64::MAX)?;
                if end > start && candidate_bytes > target {
                    break;
                }
                let candidate_bytes =
                    self.ordinary_work_bytes(candidate_encoded_bytes, candidate_output_bytes)?;
                chunk_output_bytes = candidate_output_bytes;
                chunk_bytes = candidate_bytes;
                end += 1;
                if chunk_bytes >= target {
                    break;
                }
            }

            let _reservation = self.admission.reserve(chunk_bytes, control)?;
            let chunk = decode(&reqs[start..end])?;
            if chunk.len() != end - start {
                return Err(WsiError::BackendContract {
                    context,
                    expected: end - start,
                    actual: chunk.len(),
                });
            }
            for tile in &chunk {
                validate(tile)?;
            }
            output.extend(chunk);
            start = end;
        }
        Ok(output)
    }

    fn estimate_tile_output_bytes(&self, req: &TileRequest) -> Result<u64, WsiError> {
        let scene =
            self.dataset()
                .scenes
                .get(req.scene.get())
                .ok_or(WsiError::SceneOutOfRange {
                    index: req.scene.get(),
                    count: self.dataset().scenes.len(),
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
                    count: u32::try_from(series.levels.len()).unwrap_or(u32::MAX),
                })?;
        let dimensions = match &level.tile_layout {
            TileLayout::Regular {
                tile_width,
                tile_height,
                ..
            } => (*tile_width, *tile_height),
            TileLayout::WholeLevel {
                virtual_tile_width,
                virtual_tile_height,
                ..
            } => (*virtual_tile_width, *virtual_tile_height),
            TileLayout::Irregular { tiles, .. } => tiles
                .get(&(req.col, req.row))
                .map(|entry| entry.dimensions)
                .ok_or_else(|| WsiError::TileRead {
                    level: req.level.get(),
                    col: req.col,
                    row: req.row,
                    reason: "tile not found".into(),
                })?,
        };
        let channels = u64::try_from(series.channels.len().max(4)).unwrap_or(u64::MAX);
        let output_bytes = u64::from(dimensions.0)
            .checked_mul(u64::from(dimensions.1))
            .and_then(|pixels| pixels.checked_mul(channels))
            .and_then(|samples| samples.checked_mul(series.sample_type.byte_size() as u64))
            .ok_or(WsiError::ResourceLimit {
                resource: "decoded tile/associated output",
                requested: u64::MAX,
                limit: self.limits.decoded_output_bytes(),
            })?;
        self.check_output_limit(output_bytes, "decoded tile/associated output")?;
        Ok(output_bytes)
    }

    fn estimate_associated_output_bytes(&self, name: &str) -> Result<u64, WsiError> {
        let Some(image) = self.dataset().associated_images.get(name) else {
            return Ok(0);
        };
        u64::from(image.dimensions.0)
            .checked_mul(u64::from(image.dimensions.1))
            .and_then(|pixels| pixels.checked_mul(u64::from(image.channels)))
            .and_then(|samples| samples.checked_mul(image.sample_type.byte_size() as u64))
            .ok_or(WsiError::ResourceLimit {
                resource: "decoded tile/associated output",
                requested: u64::MAX,
                limit: self.limits.decoded_output_bytes(),
            })
    }

    fn check_region_output(&self, req: &RegionRequest) -> Result<u64, WsiError> {
        let pixels = u64::from(req.size_px.0)
            .checked_mul(u64::from(req.size_px.1))
            .ok_or(WsiError::ResourceLimit {
                resource: "region pixels",
                requested: u64::MAX,
                limit: self.limits.region_pixels(),
            })?;
        if pixels > self.limits.region_pixels() {
            return Err(WsiError::ResourceLimit {
                resource: "region pixels",
                requested: pixels,
                limit: self.limits.region_pixels(),
            });
        }
        let bytes = pixels.checked_mul(4).ok_or(WsiError::ResourceLimit {
            resource: "region RGBA output",
            requested: u64::MAX,
            limit: self.limits.region_rgba_bytes(),
        })?;
        if bytes > self.limits.region_rgba_bytes() {
            return Err(WsiError::ResourceLimit {
                resource: "region RGBA output",
                requested: bytes,
                limit: self.limits.region_rgba_bytes(),
            });
        }
        Ok(bytes)
    }

    fn region_source_work(
        &self,
        req: &RegionRequest,
        fractional_origin: Option<(f64, f64)>,
    ) -> Result<(u64, u64), WsiError> {
        let Some(level) = self
            .dataset()
            .scenes
            .get(req.scene.get())
            .and_then(|scene| scene.series.get(req.series.get()))
            .and_then(|series| series.levels.get(req.level.get() as usize))
        else {
            return Ok((0, 0));
        };
        let hits = match fractional_origin {
            Some((x, y)) => {
                level
                    .tile_layout
                    .tiles_for_fractional_region(x, y, req.size_px.0, req.size_px.1)
            }
            None => level.tile_layout.tiles_for_region(
                req.origin_px.0,
                req.origin_px.1,
                req.size_px.0,
                req.size_px.1,
            ),
        };
        hits.into_iter()
            .try_fold((0_u64, 0_u64), |(total, largest), hit| {
                let tile_req = TileRequest {
                    scene: req.scene,
                    series: req.series,
                    level: req.level,
                    plane: req.plane,
                    col: hit.col,
                    row: hit.row,
                };
                let bytes = self.estimate_tile_output_bytes(&tile_req)?;
                let total = total.checked_add(bytes).ok_or(WsiError::ResourceLimit {
                    resource: "per-operation transient work",
                    requested: u64::MAX,
                    limit: self.limits.operation_transient_bytes(),
                })?;
                Ok((total, largest.max(bytes)))
            })
    }

    fn region_work_bytes(
        &self,
        encoded_bytes: u64,
        output_bytes: u64,
        largest_source_bytes: u64,
        fractional: bool,
    ) -> Result<u64, WsiError> {
        let staging = output_bytes.max(largest_source_bytes);
        let mut requested = encoded_bytes
            .checked_add(output_bytes)
            .and_then(|bytes| bytes.checked_add(staging))
            .unwrap_or(u64::MAX);
        if fractional {
            requested = requested.saturating_add(output_bytes);
        }
        if requested > self.limits.operation_transient_bytes() {
            return Err(WsiError::ResourceLimit {
                resource: "per-operation transient work",
                requested,
                limit: self.limits.operation_transient_bytes(),
            });
        }
        Ok(requested)
    }

    fn ordinary_work_bytes(&self, encoded_bytes: u64, output_bytes: u64) -> Result<u64, WsiError> {
        ReadWork::new(encoded_bytes, output_bytes)
            .ordinary_bytes(self.limits.operation_transient_bytes())
    }

    fn check_output_limit(&self, bytes: u64, resource: &'static str) -> Result<(), WsiError> {
        if bytes > self.limits.decoded_output_bytes() {
            Err(WsiError::ResourceLimit {
                resource,
                requested: bytes,
                limit: self.limits.decoded_output_bytes(),
            })
        } else {
            Ok(())
        }
    }

    fn validate_decoded_output(
        &self,
        tile: &CpuTile,
        resource: &'static str,
    ) -> Result<(), WsiError> {
        tile.validate_invariants()?;
        self.check_output_limit(
            u64::try_from(tile.data().byte_size()).unwrap_or(u64::MAX),
            resource,
        )
    }

    fn validate_encoded_unit(&self, byte_len: usize) -> Result<(), WsiError> {
        let requested = u64::try_from(byte_len).unwrap_or(u64::MAX);
        if requested > self.limits.encoded_unit_bytes() {
            Err(WsiError::ResourceLimit {
                resource: "encoded tile/frame unit",
                requested,
                limit: self.limits.encoded_unit_bytes(),
            })
        } else {
            Ok(())
        }
    }

    pub(crate) fn validate_encoded_contract(
        &self,
        byte_len: usize,
        promised: u64,
        context: &'static str,
    ) -> Result<(), WsiError> {
        let actual = u64::try_from(byte_len).unwrap_or(u64::MAX);
        if actual > promised {
            return Err(WsiError::BackendContract {
                context,
                expected: usize::try_from(promised).unwrap_or(usize::MAX),
                actual: byte_len,
            });
        }
        self.validate_encoded_unit(byte_len)
    }

    /// Read an associated image (label, macro, thumbnail).
    /// Direct delegation to the underlying SlideReader. No caching.
    pub fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let output_bytes = self.estimate_associated_output_bytes(name)?;
        self.check_output_limit(output_bytes, "decoded tile/associated output")?;
        let encoded = self.source.associated_encoded_upper_bound(name)?;
        let _reservation = self
            .admission
            .reserve(self.ordinary_work_bytes(encoded, output_bytes)?, None)?;
        let tile = self.source.read_associated(name)?;
        self.validate_decoded_output(&tile, "decoded associated image")?;
        Ok(tile)
    }
}

fn checked_rgba_bytes(width: u32, height: u32, resource: &'static str) -> Result<u64, WsiError> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(WsiError::ResourceLimit {
            resource,
            requested: u64::MAX,
            limit: u64::MAX,
        })
}

fn validate_dataset_limits(dataset: &Dataset, limits: SlideLimits) -> Result<(), WsiError> {
    let mut aggregate = 0_u64;
    let mut add_metadata = |bytes: u64| -> Result<(), WsiError> {
        if bytes > limits.metadata_value_bytes() {
            return Err(WsiError::ResourceLimit {
                resource: "individual metadata value",
                requested: bytes,
                limit: limits.metadata_value_bytes(),
            });
        }
        aggregate = aggregate.saturating_add(bytes);
        if aggregate > limits.aggregate_metadata_bytes() {
            return Err(WsiError::ResourceLimit {
                resource: "aggregate metadata",
                requested: aggregate,
                limit: limits.aggregate_metadata_bytes(),
            });
        }
        Ok(())
    };

    for (key, value) in dataset.properties.iter() {
        add_metadata(u64::try_from(key.len()).unwrap_or(u64::MAX))?;
        add_metadata(u64::try_from(value.len()).unwrap_or(u64::MAX))?;
    }
    for scene in &dataset.scenes {
        add_metadata(u64::try_from(scene.id.len()).unwrap_or(u64::MAX))?;
        if let Some(name) = &scene.name {
            add_metadata(u64::try_from(name.len()).unwrap_or(u64::MAX))?;
        }
        for series in &scene.series {
            add_metadata(u64::try_from(series.id.len()).unwrap_or(u64::MAX))?;
            for channel in &series.channels {
                if let Some(name) = &channel.name {
                    add_metadata(u64::try_from(name.len()).unwrap_or(u64::MAX))?;
                }
            }
        }
    }
    for (name, image) in &dataset.associated_images {
        add_metadata(u64::try_from(name.len()).unwrap_or(u64::MAX))?;
        add_metadata(u64::try_from(image.icc_profile.len()).unwrap_or(u64::MAX))?;
    }
    for profile in dataset.icc_profiles.values() {
        add_metadata(u64::try_from(profile.len()).unwrap_or(u64::MAX))?;
    }
    for profile in &dataset.source_icc_profiles {
        add_metadata(u64::try_from(profile.bytes.len()).unwrap_or(u64::MAX))?;
        match &profile.provenance {
            IccProfileProvenance::DicomOpticalPath {
                sop_instance_uid,
                optical_path_identifier,
            } => {
                add_metadata(u64::try_from(sop_instance_uid.len()).unwrap_or(u64::MAX))?;
                if let Some(identifier) = optical_path_identifier {
                    add_metadata(u64::try_from(identifier.len()).unwrap_or(u64::MAX))?;
                }
            }
            IccProfileProvenance::ReaderMetadata { source } => {
                add_metadata(u64::try_from(source.len()).unwrap_or(u64::MAX))?;
            }
            IccProfileProvenance::TiffTag { .. } => {}
        }
    }

    let mut index_bytes = 0_u64;
    for scene in &dataset.scenes {
        for series in &scene.series {
            for level in &series.levels {
                let entries = match &level.tile_layout {
                    TileLayout::Regular {
                        tiles_across,
                        tiles_down,
                        ..
                    } => tiles_across.checked_mul(*tiles_down).unwrap_or(u64::MAX),
                    TileLayout::WholeLevel { .. } => 1,
                    TileLayout::Irregular { tiles, .. } => {
                        u64::try_from(tiles.len()).unwrap_or(u64::MAX)
                    }
                };
                index_bytes = index_bytes.saturating_add(entries.saturating_mul(64));
                if index_bytes > limits.tile_index_bytes() {
                    return Err(WsiError::ResourceLimit {
                        resource: "tile/frame index",
                        requested: index_bytes,
                        limit: limits.tile_index_bytes(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn valid_subpixel_offset(value: f64) -> bool {
    value.is_finite() && (0.0..1.0).contains(&value)
}

#[cfg(test)]
mod tests;
