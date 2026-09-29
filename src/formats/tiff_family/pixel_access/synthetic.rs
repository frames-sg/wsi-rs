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
        self.synthetic_level_cache.max_bytes() >= bytes
    }

    pub(super) fn try_decode_synthetic_level_with_j2k(
        &self,
        req: &TileRequest,
        base_level: u32,
        factor: u32,
    ) -> Result<Option<CpuTile>, WsiError> {
        let Some(scale) = j2k_downscale_for_factor(factor) else {
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
        let options = j2k_decode_options(
            self.tiff_jpeg_decode_options_for_data(*ifd_id, false, &jpeg, None)
                .color_transform,
        );
        let view = J2kJpegView::parse_with_options(&jpeg, options)
            .map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let decoder =
            J2kJpegDecoder::from_view(view).map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let source_dims = decoder.info().dimensions;
        let scale_denom = scale.denominator();
        let scaled_width = source_dims.0.div_ceil(scale_denom);
        let scaled_height = source_dims.1.div_ceil(scale_denom);
        let (pixels, _outcome) = decoder
            .decode_request(J2kJpegDecodeRequest::scaled(J2kPixelFormat::Rgb8, scale))
            .map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let scaled = cpu_tile_from_rgb_pixels(scaled_width, scaled_height, pixels)?;

        if scaled.width == target.dimensions.0 as u32 && scaled.height == target.dimensions.1 as u32
        {
            Ok(Some(scaled))
        } else {
            Ok(None)
        }
    }

    /// Reads `rect` (`x, y, width, height` in synthetic-level pixels) of a
    /// synthetic level whose base is a restart-marker NDPI level.
    ///
    /// OpenSlide derives these levels by decoding each restart interval with
    /// libjpeg `scale_denom`, and only when the interval divides evenly by the
    /// factor. Box-filtering full-resolution pixels differs from that by up to
    /// 3 levels, so this assembles DCT-scaled strips instead. Other bases and
    /// layouts return `None` and keep the box-filter path. Strips are shared
    /// through `cache` under the base level's strip grid.
    ///
    /// Bands decode on the calling thread. A caller holding a single-flight
    /// claim therefore never waits on stolen work that sits beneath another
    /// waiter, and concurrent region reads do not queue in the shared pool.
    pub(super) fn try_read_synthetic_rect_from_scaled_ndpi_strips(
        &self,
        cache: Option<&crate::core::cache::TileCache>,
        base_tile_req: &TileRequest,
        factor: u32,
        rect: (u32, u32, u32, u32),
    ) -> Result<Option<CpuTile>, WsiError> {
        let (rect_x, rect_y, rect_w, rect_h) = rect;
        if !matches!(factor, 2 | 4 | 8) || rect_w == 0 || rect_h == 0 {
            return Ok(None);
        }
        let TileSource::NdpiJpeg {
            ifd_id,
            jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            strip_offset,
            strip_byte_count,
            ..
        } = self.tile_source_for(base_tile_req)?
        else {
            return Ok(None);
        };
        let base = &self.layout.dataset.scenes[base_tile_req.scene.get()].series
            [base_tile_req.series.get()]
        .levels[base_tile_req.level.get() as usize];
        let TileLayout::WholeLevel {
            virtual_tile_width: base_strip_w,
            virtual_tile_height: base_strip_h,
            ..
        } = base.tile_layout
        else {
            return Ok(None);
        };
        let (Ok(base_w), Ok(base_h)) = (
            u32::try_from(base.dimensions.0),
            u32::try_from(base.dimensions.1),
        ) else {
            return Ok(None);
        };
        if base_strip_w == 0
            || base_strip_h == 0
            || !base_strip_w.is_multiple_of(factor)
            || !base_strip_h.is_multiple_of(factor)
            || *tiles_across == 0
            || *tiles_down == 0
        {
            return Ok(None);
        }
        let (strip_w, strip_h) = (base_strip_w / factor, base_strip_h / factor);
        let (Some(rect_x1), Some(rect_y1)) =
            (rect_x.checked_add(rect_w), rect_y.checked_add(rect_h))
        else {
            return Ok(None);
        };
        let (col_start, row_start) = (rect_x / strip_w, rect_y / strip_h);
        let col_end = ((rect_x1 - 1) / strip_w).min(tiles_across - 1);
        let row_end = ((rect_y1 - 1) / strip_h).min(tiles_down - 1);
        if col_start > col_end || row_start > row_end {
            return Ok(None);
        }

        // One task per strip row writes its own band of output rows, so strip
        // decodes need no batch barrier or intermediate collection.
        let dst_stride = rect_w as usize * 3;
        let mut rgb = vec![255u8; checked_rgb_u8_len(rect_w, rect_h)?];
        let mut bands = Vec::with_capacity((row_end - row_start + 1) as usize);
        let mut rest = rgb.as_mut_slice();
        for row in row_start..=row_end {
            let band_y1 = ((row + 1) * strip_h).min(rect_y1);
            let band_y0 = (row * strip_h).max(rect_y);
            let (band, tail) = rest.split_at_mut((band_y1 - band_y0) as usize * dst_stride);
            bands.push((row, band_y0, band));
            rest = tail;
        }
        let decode_band = |(row, band_y0, band): (u32, u32, &mut [u8])| {
            for col in col_start..=col_end {
                let strip = self.scaled_ndpi_strip(
                    cache,
                    &TileRequest {
                        col: i64::from(col),
                        row: i64::from(row),
                        ..*base_tile_req
                    },
                    ScaledNdpiStrip {
                        ifd_id: *ifd_id,
                        jpeg_header,
                        mcu_starts_tag: *mcu_starts_tag,
                        tiles_across: *tiles_across,
                        tiles_down: *tiles_down,
                        strip_offset: *strip_offset,
                        strip_byte_count: *strip_byte_count,
                        base_strip: (base_strip_w, base_strip_h),
                        base_dims: (base_w, base_h),
                        factor,
                    },
                )?;
                let (CpuTileLayout::Interleaved, 3, CpuTileData::U8(strip_rgb)) =
                    (strip.layout, strip.channels, &strip.data)
                else {
                    return Err(WsiError::TileRead {
                        col: i64::from(col),
                        row: i64::from(row),
                        level: base_tile_req.level.get(),
                        reason: "scaled NDPI strip must be interleaved U8 RGB".into(),
                    });
                };
                let (origin_x, origin_y) = (col * strip_w, row * strip_h);
                let x0 = origin_x.max(rect_x);
                let x1 = (origin_x + strip.width).min(rect_x1);
                let y1 = (origin_y + strip.height).min(band_y0 + (band.len() / dst_stride) as u32);
                if x1 <= x0 || y1 <= band_y0 {
                    continue;
                }
                let src_stride = strip.width as usize * 3;
                let row_bytes = (x1 - x0) as usize * 3;
                for y in band_y0..y1 {
                    let src = (y - origin_y) as usize * src_stride + (x0 - origin_x) as usize * 3;
                    let dst = (y - band_y0) as usize * dst_stride + (x0 - rect_x) as usize * 3;
                    band[dst..dst + row_bytes].copy_from_slice(&strip_rgb[src..src + row_bytes]);
                }
            }
            Ok(())
        };
        bands.into_iter().try_for_each(decode_band)?;
        Ok(Some(cpu_tile_from_rgb_pixels(rect_w, rect_h, rgb)?))
    }

    /// One DCT-scaled restart-interval strip, shared through `cache`. It skips
    /// the small single-flight NDPI strip cache: scaled strips are numerous and
    /// tiny, and churning that LRU would evict full-resolution strips.
    fn scaled_ndpi_strip(
        &self,
        cache: Option<&crate::core::cache::TileCache>,
        strip_req: &TileRequest,
        strip: ScaledNdpiStrip<'_>,
    ) -> Result<Arc<CpuTile>, WsiError> {
        let cache_key = CacheKey {
            kind: CacheKeyKind::NdpiScaledStrip {
                scale_denom: strip.factor,
            },
            ..CacheKey::from_tile_request(self.layout.dataset.id, strip_req)
        };
        if let Some(cached) = cache.and_then(|cache| cache.get(&cache_key)) {
            return Ok(cached);
        }
        let (col, native_row) =
            validate_tile_coords(strip_req.col, strip_req.row, strip_req.level.get())?;
        let decoded = self.decode_ndpi_strip(
            strip_req,
            strip.ifd_id,
            strip.jpeg_header,
            strip.mcu_starts_tag,
            strip.tiles_across,
            strip.tiles_down,
            strip.strip_offset,
            strip.strip_byte_count,
            NdpiStripKey {
                ifd_id: strip.ifd_id,
                col,
                native_row,
                scale_denom: strip.factor,
            },
            strip.base_strip.0,
            strip.base_strip.1,
            strip.base_dims.0,
            strip.base_dims.1,
        )?;
        if let Some(cache) = cache {
            cache.put(cache_key, decoded.clone());
        }
        Ok(decoded)
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

        if let Some(image) = self.try_decode_synthetic_level_with_j2k(req, base_level, factor)? {
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
        if let (Ok(target_w), Ok(target_h)) = (
            u32::try_from(target.dimensions.0),
            u32::try_from(target.dimensions.1),
        ) {
            // `get_or_decode_synthetic_level` holds this level's single-flight
            // claim while this runs.
            if let Some(image) = self.try_read_synthetic_rect_from_scaled_ndpi_strips(
                None,
                &base_tile_req,
                factor,
                (0, 0, target_w, target_h),
            )? {
                return Ok(Arc::new(image));
            }
        }
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

        let key = CacheKey::from_region_tile(self.layout.dataset.id, req, 0, 0);
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
            self.try_decode_synthetic_level_with_j2k(&tile_req, base_level, factor)?
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

        // Clipping can only shrink the u32-sized request, so these deltas fit u32.
        let valid_w = (clipped_x1 - clipped_x0) as u32;
        let valid_h = (clipped_y1 - clipped_y0) as u32;
        let dst_x = (clipped_x0 - x0) as u32;
        let dst_y = (clipped_y0 - y0) as u32;

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
                self.try_decode_synthetic_level_with_j2k(&tile_req, base_level, factor)?
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
        if let (Ok(rect_x), Ok(rect_y)) = (u32::try_from(clipped_x0), u32::try_from(clipped_y0)) {
            if let Some(scaled) = self.try_read_synthetic_rect_from_scaled_ndpi_strips(
                cache,
                &base_tile_req,
                factor,
                (rect_x, rect_y, valid_w, valid_h),
            )? {
                return paste_rgb_interleaved_u8_tile(&scaled, w, h, dst_x, dst_y);
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
        // Each coordinate was clamped to the nonnegative u64 target dimensions.
        let clipped_x0 = clipped_x0 as u128;
        let clipped_y0 = clipped_y0 as u128;
        let clipped_x1 = clipped_x1 as u128;
        let clipped_y1 = clipped_y1 as u128;
        let factor = u128::from(factor);
        // A u64 coordinate multiplied by a u32 factor cannot overflow u128.
        let base_x0 = clipped_x0 * factor;
        let base_y0 = clipped_y0 * factor;
        let base_x1 = (clipped_x1 * factor).min(u128::from(base.dimensions.0));
        let base_y1 = (clipped_y1 * factor).min(u128::from(base.dimensions.1));
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
        let base_region = ensure_interleaved_rgb_u8(composite_region_from_source(
            self,
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
        self.synthetic_level_cache
            .get_or_try_insert_with(key, || {
                self.decode_synthetic_level(req, base_level, factor)
                    .map_err(|err| err.to_string())
            })
            .map_err(|reason| Self::ndpi_full_decode_error(req, reason))
    }
}

/// The base NDPI restart-marker level and scale of one DCT-scaled strip.
#[derive(Clone, Copy)]
struct ScaledNdpiStrip<'a> {
    ifd_id: IfdId,
    jpeg_header: &'a [u8],
    mcu_starts_tag: u16,
    tiles_across: u32,
    tiles_down: u32,
    strip_offset: u64,
    strip_byte_count: u64,
    base_strip: (u32, u32),
    base_dims: (u32, u32),
    factor: u32,
}
