use super::dispatch::TiffPixelReaderNoSyntheticPrime;
use super::*;

impl TiffPixelReader {
    pub(super) fn synthetic_level_key_for_region(
        req: &RegionRequest,
        base_level: u32,
    ) -> SyntheticLevelKey {
        let plane = req.plane.get();
        SyntheticLevelKey {
            scene: req.scene.get(),
            series: req.series.get(),
            base_level,
            target_level: req.level.get(),
            z: plane.z,
            c: plane.c,
            t: plane.t,
        }
    }

    pub(super) fn synthetic_level_key_for_tile(
        req: &TileRequest,
        base_level: u32,
    ) -> SyntheticLevelKey {
        SyntheticLevelKey {
            scene: req.scene.get(),
            series: req.series.get(),
            base_level,
            target_level: req.level.get(),
            z: req.plane.get().z,
            c: req.plane.get().c,
            t: req.plane.get().t,
        }
    }

    pub(super) fn get_cached_synthetic_level(
        &self,
        key: &SyntheticLevelKey,
    ) -> Option<Arc<CpuTile>> {
        self.synthetic_region_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
    }

    pub(super) fn put_synthetic_level_cache(&self, key: SyntheticLevelKey, image: Arc<CpuTile>) {
        self.synthetic_region_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(key, image);
    }

    pub(super) fn synthetic_level_cache_can_hold(&self, dimensions: (u64, u64)) -> bool {
        let Some(bytes) = dimensions
            .0
            .checked_mul(dimensions.1)
            .and_then(|pixels| pixels.checked_mul(3))
        else {
            return false;
        };
        self.synthetic_level_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .max_bytes
            >= bytes
    }

    pub(super) fn try_decode_synthetic_level_with_signinum(
        &self,
        req: &TileRequest,
        base_level: u32,
        factor: u32,
    ) -> Result<Option<CpuTile>, WsiError> {
        let Some(scale) = signinum_downscale_for_factor(factor) else {
            return Ok(None);
        };
        let target = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let base_req = TileRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: base_level.into(),
            plane: req.plane,
            col: 0,
            row: 0,
        };
        let TileSource::NdpiFullDecode {
            ifd_id,
            strip_offset,
            strip_byte_count,
            ..
        } = self.tile_source_for(&base_req)?
        else {
            return Ok(None);
        };

        let jpeg = self
            .container
            .pread(*strip_offset, *strip_byte_count)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;
        let options = signinum_decode_options(
            self.tiff_jpeg_decode_options_for_data(*ifd_id, false, &jpeg, None)
                .color_transform,
        );
        let decoder = SigninumJpegDecoder::new_with_options(&jpeg, options)
            .map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let source_dims = decoder.info().dimensions;
        let scale_denom = scale.denominator();
        let scaled_width = source_dims.0.div_ceil(scale_denom);
        let scaled_height = source_dims.1.div_ceil(scale_denom);
        let (pixels, _outcome) = decoder
            .decode_scaled(SigninumPixelFormat::Rgb8, scale)
            .map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let scaled = cpu_tile_from_rgb_pixels(scaled_width, scaled_height, pixels)?;

        if scaled.width == target.dimensions.0 as u32 && scaled.height == target.dimensions.1 as u32
        {
            Ok(Some(scaled))
        } else {
            Ok(None)
        }
    }

    pub(super) fn prime_deepest_synthetic_levels_best_effort(&self) {
        let mut deepest: HashMap<SyntheticDeepestKey, SyntheticDeepestValue> = HashMap::new();
        for (key, source) in &self.layout.tile_sources {
            let TileSource::SyntheticDownsample { base_level, factor } = source else {
                continue;
            };
            deepest
                .entry((key.scene, key.series, key.z, key.c, key.t))
                .and_modify(|current| {
                    if key.level > current.0 {
                        *current = (key.level, *base_level, *factor);
                    }
                })
                .or_insert((key.level, *base_level, *factor));
        }

        for ((scene, series, z, c, t), (target_level, base_level, factor)) in deepest {
            let req = TileRequest {
                scene: scene.into(),
                series: series.into(),
                level: target_level.into(),
                plane: PlaneSelection { z, c, t }.into(),
                col: 0,
                row: 0,
            };
            let key = Self::synthetic_level_key_for_tile(&req, base_level);
            if self.get_cached_synthetic_level(&key).is_some() {
                continue;
            }
            if let Ok(Some(image)) =
                self.try_decode_synthetic_level_with_signinum(&req, base_level, factor)
            {
                self.put_synthetic_level_cache(key, Arc::new(image));
            }
        }
    }

    pub(super) fn decode_synthetic_level(
        &self,
        req: &TileRequest,
        base_level: u32,
        factor: u32,
    ) -> Result<Arc<CpuTile>, WsiError> {
        if !factor.is_power_of_two() || factor < 2 {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("invalid synthetic NDPI factor {factor}"),
            });
        }

        if let Some(image) =
            self.try_decode_synthetic_level_with_signinum(req, base_level, factor)?
        {
            return Ok(Arc::new(image));
        }

        let base = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [base_level as usize];
        let target = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let base_tile_req = TileRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: base_level.into(),
            plane: req.plane,
            col: 0,
            row: 0,
        };
        let mut current = if matches!(
            self.tile_source_for(&base_tile_req),
            Ok(TileSource::NdpiFullDecode { .. })
        ) {
            self.read_tile_cpu(&base_tile_req)?
        } else {
            composite_region_from_source(
                self,
                None,
                &RegionRequest {
                    scene: req.scene,
                    series: req.series,
                    level: LevelIdx::new(base_level),
                    plane: req.plane,
                    origin_px: (0, 0),
                    size_px: (
                        u32::try_from(base.dimensions.0).unwrap_or(u32::MAX),
                        u32::try_from(base.dimensions.1).unwrap_or(u32::MAX),
                    ),
                },
                DEFAULT_MAX_REGION_PIXELS,
            )?
        };

        if current.layout != CpuTileLayout::Interleaved
            || current.channels != 3
            || current.color_space != ColorSpace::Rgb
            || current.data.as_u8().is_none()
        {
            current = rgba_image_to_sample_buffer(current.to_rgba()?);
        }

        current = fit_synthetic_rgb_tile_to_dimensions(
            downsample_rgb_pow2_box(&current, factor)?,
            target.dimensions.0 as u32,
            target.dimensions.1 as u32,
        )?;

        Ok(Arc::new(current))
    }

    pub(super) fn read_full_synthetic_region_fastpath(
        &self,
        cache: Option<&crate::core::cache::TileCache>,
        req: &RegionRequest,
        base_level: u32,
        factor: u32,
        max_region_pixels: u64,
    ) -> Result<CpuTile, WsiError> {
        if !factor.is_power_of_two() || !(2..=8).contains(&factor) {
            return composite_region_from_source(self, cache, req, max_region_pixels);
        }

        let (x, y) = req.origin_px;
        let (w, h) = req.size_px;
        let plane = req.plane.get();
        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        if x != 0
            || y != 0
            || u64::from(w) != level.dimensions.0
            || u64::from(h) != level.dimensions.1
        {
            return self.read_synthetic_subregion_fastpath(
                cache,
                req,
                base_level,
                factor,
                level.dimensions,
                max_region_pixels,
            );
        }

        let key = CacheKey {
            dataset_id: self.layout.dataset.id,
            scene: req.scene.get() as u32,
            series: req.series.get() as u32,
            level: req.level.get(),
            z: plane.z,
            c: plane.c,
            t: plane.t,
            tile_col: 0,
            tile_row: 0,
        };
        if let Some(cache) = cache {
            if let Some(cached) = cache.get(&key) {
                return Ok(cached.as_ref().clone());
            }
        }

        let synthetic_key = Self::synthetic_level_key_for_region(req, base_level);
        if let Some(cached) = self.get_cached_synthetic_level(&synthetic_key) {
            if let Some(cache) = cache {
                cache.put(key, cached.clone());
            }
            return Ok(cached.as_ref().clone());
        }

        let base_req = TileRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: base_level.into(),
            plane: plane.into(),
            col: 0,
            row: 0,
        };
        let TileSource::NdpiFullDecode {
            ifd_id,
            strip_offset,
            strip_byte_count,
            ..
        } = self.tile_source_for(&base_req)?
        else {
            return composite_region_from_source(self, cache, req, max_region_pixels);
        };

        let tile_req = TileRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: req.level.get().into(),
            plane: req.plane.get().into(),
            col: 0,
            row: 0,
        };
        let scaled = if let Some(image) =
            self.try_decode_synthetic_level_with_signinum(&tile_req, base_level, factor)?
        {
            image
        } else {
            let full = self.get_or_decode_ndpi_full_image(
                &base_req,
                *ifd_id,
                *strip_offset,
                *strip_byte_count,
            )?;
            downsample_rgb_pow2_box(full.as_ref(), factor)?
        };
        let image = Arc::new(fit_synthetic_rgb_tile_to_dimensions(scaled, w, h)?);
        if image.width != w || image.height != h {
            return composite_region_from_source(self, cache, req, max_region_pixels);
        }
        self.put_synthetic_level_cache(synthetic_key, image.clone());
        if let Some(cache) = cache {
            cache.put(key, image.clone());
        }
        Ok(image.as_ref().clone())
    }

    pub(super) fn read_synthetic_subregion_fastpath(
        &self,
        cache: Option<&crate::core::cache::TileCache>,
        req: &RegionRequest,
        base_level: u32,
        factor: u32,
        target_dimensions: (u64, u64),
        max_region_pixels: u64,
    ) -> Result<CpuTile, WsiError> {
        let (target_width, target_height) = target_dimensions;
        let (x, y) = req.origin_px;
        let (w, h) = req.size_px;
        if w == 0 || h == 0 {
            return zero_rgb_interleaved_u8_tile(w, h);
        }

        let x0 = i128::from(x);
        let y0 = i128::from(y);
        let x1 = x0 + i128::from(w);
        let y1 = y0 + i128::from(h);
        let target_w = i128::from(target_width);
        let target_h = i128::from(target_height);
        let clipped_x0 = x0.clamp(0, target_w);
        let clipped_y0 = y0.clamp(0, target_h);
        let clipped_x1 = x1.clamp(0, target_w);
        let clipped_y1 = y1.clamp(0, target_h);

        if clipped_x1 <= clipped_x0 || clipped_y1 <= clipped_y0 {
            return zero_rgb_interleaved_u8_tile(w, h);
        }

        let valid_w = u32::try_from(clipped_x1 - clipped_x0).map_err(|_| {
            WsiError::DisplayConversion(format!(
                "synthetic NDPI ROI width exceeds region API bounds: {}",
                clipped_x1 - clipped_x0
            ))
        })?;
        let valid_h = u32::try_from(clipped_y1 - clipped_y0).map_err(|_| {
            WsiError::DisplayConversion(format!(
                "synthetic NDPI ROI height exceeds region API bounds: {}",
                clipped_y1 - clipped_y0
            ))
        })?;
        let dst_x = u32::try_from(clipped_x0 - x0).map_err(|_| {
            WsiError::DisplayConversion("synthetic NDPI ROI destination x overflow".into())
        })?;
        let dst_y = u32::try_from(clipped_y0 - y0).map_err(|_| {
            WsiError::DisplayConversion("synthetic NDPI ROI destination y overflow".into())
        })?;

        let base_tile_req = TileRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: base_level.into(),
            plane: req.plane.get().into(),
            col: 0,
            row: 0,
        };
        if matches!(
            self.tile_source_for(&base_tile_req)?,
            TileSource::NdpiFullDecode { .. }
        ) {
            let tile_req = TileRequest {
                scene: req.scene.get().into(),
                series: req.series.get().into(),
                level: req.level.get().into(),
                plane: req.plane.get().into(),
                col: 0,
                row: 0,
            };
            if let Some(scaled) =
                self.try_decode_synthetic_level_with_signinum(&tile_req, base_level, factor)?
            {
                let crop_x0 = u32::try_from(clipped_x0).map_err(|_| {
                    WsiError::DisplayConversion(
                        "synthetic NDPI ROI source x exceeds crop bounds".into(),
                    )
                })?;
                let crop_y0 = u32::try_from(clipped_y0).map_err(|_| {
                    WsiError::DisplayConversion(
                        "synthetic NDPI ROI source y exceeds crop bounds".into(),
                    )
                })?;
                let cropped =
                    crop_rgb_interleaved_u8_buffer(&scaled, crop_x0, crop_y0, valid_w, valid_h)?;
                return paste_rgb_interleaved_u8_tile(&cropped, w, h, dst_x, dst_y);
            }
        }

        let series = self
            .layout
            .dataset
            .scenes
            .get(req.scene.get())
            .and_then(|scene| scene.series.get(req.series.get()))
            .ok_or_else(|| WsiError::SeriesOutOfRange {
                index: req.series.get(),
                count: self
                    .layout
                    .dataset
                    .scenes
                    .get(req.scene.get())
                    .map_or(0, |scene| scene.series.len()),
            })?;
        let base =
            series
                .levels
                .get(base_level as usize)
                .ok_or_else(|| WsiError::LevelOutOfRange {
                    level: base_level,
                    count: series.levels.len() as u32,
                })?;
        let clipped_x0 = u128::try_from(clipped_x0).map_err(|_| {
            WsiError::DisplayConversion("synthetic NDPI ROI source x is negative".into())
        })?;
        let clipped_y0 = u128::try_from(clipped_y0).map_err(|_| {
            WsiError::DisplayConversion("synthetic NDPI ROI source y is negative".into())
        })?;
        let clipped_x1 = u128::try_from(clipped_x1).map_err(|_| {
            WsiError::DisplayConversion("synthetic NDPI ROI source right is negative".into())
        })?;
        let clipped_y1 = u128::try_from(clipped_y1).map_err(|_| {
            WsiError::DisplayConversion("synthetic NDPI ROI source bottom is negative".into())
        })?;
        let factor = u128::from(factor);
        let base_x0 = clipped_x0.checked_mul(factor).ok_or_else(|| {
            WsiError::DisplayConversion("synthetic NDPI base ROI x overflow".into())
        })?;
        let base_y0 = clipped_y0.checked_mul(factor).ok_or_else(|| {
            WsiError::DisplayConversion("synthetic NDPI base ROI y overflow".into())
        })?;
        let base_x1 = clipped_x1
            .checked_mul(factor)
            .ok_or_else(|| {
                WsiError::DisplayConversion("synthetic NDPI base ROI right overflow".into())
            })?
            .min(u128::from(base.dimensions.0));
        let base_y1 = clipped_y1
            .checked_mul(factor)
            .ok_or_else(|| {
                WsiError::DisplayConversion("synthetic NDPI base ROI bottom overflow".into())
            })?
            .min(u128::from(base.dimensions.1));
        if base_x1 <= base_x0 || base_y1 <= base_y0 {
            return zero_rgb_interleaved_u8_tile(w, h);
        }

        let base_req = RegionRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: LevelIdx::new(base_level),
            plane: req.plane,
            origin_px: (
                i64::try_from(base_x0).map_err(|_| {
                    WsiError::DisplayConversion("synthetic NDPI base ROI x exceeds i64".into())
                })?,
                i64::try_from(base_y0).map_err(|_| {
                    WsiError::DisplayConversion("synthetic NDPI base ROI y exceeds i64".into())
                })?,
            ),
            size_px: (
                u32::try_from(base_x1 - base_x0).map_err(|_| {
                    WsiError::DisplayConversion(
                        "synthetic NDPI base ROI width exceeds region API bounds".into(),
                    )
                })?,
                u32::try_from(base_y1 - base_y0).map_err(|_| {
                    WsiError::DisplayConversion(
                        "synthetic NDPI base ROI height exceeds region API bounds".into(),
                    )
                })?,
            ),
        };
        let base_source = TiffPixelReaderNoSyntheticPrime { inner: self };
        let base_region = ensure_interleaved_rgb_u8(composite_region_from_source(
            &base_source,
            cache,
            &base_req,
            max_region_pixels,
        )?)?;
        let downsampled = fit_synthetic_rgb_tile_to_dimensions(
            downsample_rgb_pow2_box(&base_region, factor as u32)?,
            valid_w,
            valid_h,
        )?;
        paste_rgb_interleaved_u8_tile(&downsampled, w, h, dst_x, dst_y)
    }

    pub(super) fn read_synthetic_display_tile(
        &self,
        req: &TileViewRequest,
        base_level: u32,
        factor: u32,
    ) -> Result<CpuTile, WsiError> {
        let series = self
            .layout
            .dataset
            .scenes
            .get(req.scene.get())
            .and_then(|scene| scene.series.get(req.series.get()))
            .ok_or_else(|| WsiError::SeriesOutOfRange {
                index: req.series.get(),
                count: self
                    .layout
                    .dataset
                    .scenes
                    .get(req.scene.get())
                    .map_or(0, |scene| scene.series.len()),
            })?;
        let level = series.levels.get(req.level.get() as usize).ok_or_else(|| {
            WsiError::LevelOutOfRange {
                level: req.level.get(),
                count: series.levels.len() as u32,
            }
        })?;

        let origin_x = req.col.saturating_mul(i64::from(req.tile_width));
        let origin_y = req.row.saturating_mul(i64::from(req.tile_height));
        let level_w = i64::try_from(level.dimensions.0).unwrap_or(i64::MAX);
        let level_h = i64::try_from(level.dimensions.1).unwrap_or(i64::MAX);
        if origin_x >= level_w || origin_y >= level_h {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "display tile origin out of bounds".into(),
            });
        }
        if origin_x >= 0 && origin_y >= 0 && self.synthetic_level_cache_can_hold(level.dimensions) {
            let tile_req = TileRequest {
                scene: req.scene.get().into(),
                series: req.series.get().into(),
                level: req.level.get().into(),
                plane: req.plane,
                col: 0,
                row: 0,
            };
            let image = self.get_or_decode_synthetic_level(&tile_req, base_level, factor)?;
            let crop_width = req.tile_width.min((level_w - origin_x) as u32);
            let crop_height = req.tile_height.min((level_h - origin_y) as u32);
            return crop_rgb_interleaved_u8_buffer(
                image.as_ref(),
                origin_x as u32,
                origin_y as u32,
                crop_width,
                crop_height,
            );
        }

        let clipped = RegionRequest {
            scene: req.scene,
            series: req.series,
            level: LevelIdx::new(req.level.get()),
            plane: req.plane,
            origin_px: (origin_x, origin_y),
            size_px: (
                req.tile_width.min((level_w - origin_x) as u32),
                req.tile_height.min((level_h - origin_y) as u32),
            ),
        };
        self.read_full_synthetic_region_fastpath(
            None,
            &clipped,
            base_level,
            factor,
            DEFAULT_MAX_REGION_PIXELS,
        )
    }

    pub(super) fn get_or_decode_synthetic_level(
        &self,
        req: &TileRequest,
        base_level: u32,
        factor: u32,
    ) -> Result<Arc<CpuTile>, WsiError> {
        let key = Self::synthetic_level_key_for_tile(req, base_level);

        if let Some(image) = {
            let mut cache = self
                .synthetic_level_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.get(&key)
        } {
            return Ok(image);
        }

        let mut flights = self
            .synthetic_level_flights
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut registered_waiter = false;
        loop {
            match flights.get_mut(&key) {
                Some(flight) => {
                    if !registered_waiter {
                        flight.waiters += 1;
                        registered_waiter = true;
                    }
                    if let Some(result) = flight.result.clone() {
                        flight.waiters -= 1;
                        if flight.waiters == 0 {
                            flights.remove(&key);
                        }
                        return result.map_err(|reason| Self::ndpi_full_decode_error(req, reason));
                    }
                    flights = self
                        .synthetic_level_ready
                        .wait(flights)
                        .unwrap_or_else(|e| e.into_inner());
                }
                None if registered_waiter => {
                    return Err(Self::ndpi_full_decode_error(
                        req,
                        format!(
                            "synthetic NDPI level decode flight for {:?} disappeared",
                            key
                        ),
                    ));
                }
                None => {
                    flights.insert(key, SyntheticLevelFlight::default());
                    break;
                }
            }
        }
        drop(flights);

        let decode_result = self
            .decode_synthetic_level(req, base_level, factor)
            .map_err(|err| err.to_string());
        if let Ok(image) = decode_result.as_ref() {
            let mut cache = self
                .synthetic_level_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.put(key, image.clone());
        }

        let mut flights = self
            .synthetic_level_flights
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(flight) = flights.get_mut(&key) {
            flight.result = Some(decode_result.clone());
            if flight.waiters == 0 {
                flights.remove(&key);
            }
        }
        drop(flights);
        self.synthetic_level_ready.notify_all();

        decode_result.map_err(|reason| Self::ndpi_full_decode_error(req, reason))
    }
}
