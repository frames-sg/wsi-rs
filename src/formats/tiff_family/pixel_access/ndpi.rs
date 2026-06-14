use super::*;

impl TiffPixelReader {
    pub(super) fn get_cached_ndpi_strip(&self, strip_key: NdpiStripKey) -> Option<Arc<CpuTile>> {
        self.ndpi_strip_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&strip_key)
    }

    pub(super) fn clamp_ndpi_strip_crop(
        src_x: u32,
        src_y: u32,
        width: u32,
        height: u32,
        strip_width: u32,
        strip_height: u32,
    ) -> Option<(u32, u32)> {
        if src_x >= strip_width || src_y >= strip_height {
            return None;
        }

        let clamped_width = width.min(strip_width - src_x);
        let clamped_height = height.min(strip_height - src_y);
        if clamped_width == 0 || clamped_height == 0 {
            return None;
        }

        Some((clamped_width, clamped_height))
    }

    pub(super) fn ndpi_full_decode_error(req: &TileRequest, reason: impl Into<String>) -> WsiError {
        WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: reason.into(),
        }
    }

    pub(super) fn ndpi_restart_error_allows_full_decode_fallback(err: &WsiError) -> bool {
        let WsiError::TileRead { reason, .. } = err else {
            return false;
        };
        reason.contains("NDPI MCU segment")
            || reason.contains("NDPI MCU-starts table")
            || reason.contains("NDPI MCU-starts index")
    }

    fn ndpi_mcu_starts_are_file_absolute(
        starts: &[u64],
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> bool {
        if starts.is_empty() || strip_offset == 0 {
            return false;
        }

        let mut any_outside_relative_strip = false;
        let mut any_normalized_inside_strip = false;
        for &start in starts {
            if start < strip_offset {
                return false;
            }
            if start >= strip_byte_count {
                any_outside_relative_strip = true;
            }
            if start.saturating_sub(strip_offset) < strip_byte_count {
                any_normalized_inside_strip = true;
            }
        }

        any_outside_relative_strip && any_normalized_inside_strip
    }

    fn normalize_ndpi_mcu_start(start: u64, file_absolute: bool, strip_offset: u64) -> u64 {
        if file_absolute {
            start.saturating_sub(strip_offset)
        } else {
            start
        }
    }

    pub(super) fn ndpi_mcu_starts(
        &self,
        ifd_id: IfdId,
        mcu_starts_tag: u16,
    ) -> Result<Arc<Vec<u64>>, WsiError> {
        if let Some(starts) = self
            .ndpi_mcu_starts_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(ifd_id, mcu_starts_tag))
            .cloned()
        {
            return Ok(starts);
        }

        let starts = Arc::new(
            self.container
                .get_u64_array(ifd_id, mcu_starts_tag)
                .map_err(|e| e.into_wsi_error(self.container.path()))?
                .to_vec(),
        );
        self.ndpi_mcu_starts_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((ifd_id, mcu_starts_tag), starts.clone());
        Ok(starts)
    }

    pub(super) fn decode_ndpi_full_image(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<Arc<CpuTile>, WsiError> {
        let data = self
            .container
            .pread(strip_offset, strip_byte_count)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;

        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let (level_w, level_h) = level.dimensions;
        let options = signinum_decode_options(
            self.tiff_jpeg_decode_options_for_data(ifd_id, false, &data, None)
                .color_transform,
        );
        let decoder = SigninumJpegDecoder::new_with_options(&data, options)
            .map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let (pixels, outcome) = decoder
            .decode(SigninumPixelFormat::Rgb8)
            .map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let decoded = cpu_tile_from_rgb_pixels(outcome.decoded.w, outcome.decoded.h, pixels)?;
        let decoded = if decoded.width > level_w as u32 || decoded.height > level_h as u32 {
            crop_rgb_interleaved_u8_buffer(&decoded, 0, 0, level_w as u32, level_h as u32)?
        } else {
            decoded
        };

        Ok(Arc::new(decoded))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn read_ndpi_display_tile(
        &self,
        req: &TileViewRequest,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        mcu_starts_tag: u16,
        tiles_across: u32,
        tiles_down: u32,
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<CpuTile, WsiError> {
        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let (level_w, level_h) = level.dimensions;
        let (vtw, vth) = match &level.tile_layout {
            TileLayout::WholeLevel {
                virtual_tile_width,
                virtual_tile_height,
                ..
            } => (*virtual_tile_width, *virtual_tile_height),
            _ => {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NdpiJpeg display tile expects WholeLevel layout".into(),
                });
            }
        };

        let tile_origin_x = req.col.saturating_mul(i64::from(req.tile_width));
        let tile_origin_y = req.row.saturating_mul(i64::from(req.tile_height));
        let level_w = level_w as i64;
        let level_h = level_h as i64;
        if tile_origin_x < 0
            || tile_origin_y < 0
            || tile_origin_x >= level_w
            || tile_origin_y >= level_h
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "display tile origin out of bounds".into(),
            });
        }

        let content_width = req.tile_width.min((level_w - tile_origin_x) as u32);
        let content_height = req.tile_height.min((level_h - tile_origin_y) as u32);
        let tile_end_x = tile_origin_x as u32 + content_width;
        let tile_end_y = tile_origin_y as u32 + content_height;
        let native_col_start = tile_origin_x as u32 / vtw;
        let native_col_end = tile_end_x.saturating_sub(1) / vtw;
        let tile_origin_y_u32 = tile_origin_y as u32;
        let native_row_start = tile_origin_y_u32 / vth;
        let native_row_end = tile_end_y.saturating_sub(1) / vth;

        struct NeededNdpiStrip {
            strip_key: NdpiStripKey,
            strip_req: TileRequest,
            copy_start_x: u32,
            copy_start_y: u32,
            copy_width: u32,
            copy_height: u32,
            dest_x: u32,
            dest_y: u32,
            strip: Option<Arc<CpuTile>>,
        }

        let mut needed_strips = Vec::new();
        for native_row in native_row_start..=native_row_end {
            let strip_origin_y = native_row * vth;

            for native_col in native_col_start..=native_col_end {
                let strip_origin_x = native_col * vtw;
                let strip_width = vtw.min((level_w as u32).saturating_sub(strip_origin_x));
                let strip_height = vth.min((level_h as u32).saturating_sub(strip_origin_y));
                let copy_start_x = (tile_origin_x as u32).saturating_sub(strip_origin_x);
                let copy_start_y = tile_origin_y_u32.saturating_sub(strip_origin_y);
                let copy_end_x = tile_end_x.min(strip_origin_x + strip_width);
                let copy_end_y = tile_end_y.min(strip_origin_y + strip_height);
                let desired_width = copy_end_x.saturating_sub(strip_origin_x + copy_start_x);
                let desired_height = copy_end_y.saturating_sub(strip_origin_y + copy_start_y);
                let Some((copy_width, copy_height)) = Self::clamp_ndpi_strip_crop(
                    copy_start_x,
                    copy_start_y,
                    desired_width,
                    desired_height,
                    strip_width,
                    strip_height,
                ) else {
                    continue;
                };

                let strip_key = NdpiStripKey {
                    ifd_id,
                    col: native_col,
                    native_row,
                };
                let dest_x = strip_origin_x
                    .saturating_add(copy_start_x)
                    .saturating_sub(tile_origin_x as u32);
                let dest_y = strip_origin_y
                    .saturating_add(copy_start_y)
                    .saturating_sub(tile_origin_y as u32);
                needed_strips.push(NeededNdpiStrip {
                    strip_key,
                    strip_req: TileRequest {
                        scene: req.scene.get().into(),
                        series: req.series.get().into(),
                        level: req.level.get().into(),
                        plane: req.plane,
                        col: i64::from(native_col),
                        row: i64::from(native_row),
                    },
                    copy_start_x,
                    copy_start_y,
                    copy_width,
                    copy_height,
                    dest_x,
                    dest_y,
                    strip: self.get_cached_ndpi_strip(strip_key),
                });
            }
        }

        let missing_indices: Vec<usize> = needed_strips
            .iter()
            .enumerate()
            .filter_map(|(idx, needed)| needed.strip.is_none().then_some(idx))
            .collect();
        if !missing_indices.is_empty() {
            let decode_batch = if vtw > NDPI_DISPLAY_WIDE_STRIP_WIDTH {
                NDPI_DISPLAY_WIDE_STRIP_BATCH
            } else {
                NDPI_DISPLAY_NARROW_STRIP_BATCH
            };
            let decoded_missing: Result<Vec<(usize, Arc<CpuTile>)>, WsiError> =
                if missing_indices.len() == 1 {
                    let idx = missing_indices[0];
                    let needed = &needed_strips[idx];
                    Ok(vec![(
                        idx,
                        self.get_or_decode_ndpi_strip(
                            &needed.strip_req,
                            ifd_id,
                            jpeg_header,
                            mcu_starts_tag,
                            tiles_across,
                            tiles_down,
                            strip_offset,
                            strip_byte_count,
                            needed.strip_key,
                            vtw,
                            vth,
                            level_w as u32,
                            level_h as u32,
                        )?,
                    )])
                } else {
                    let mut decoded = Vec::with_capacity(missing_indices.len());
                    for batch in missing_indices.chunks(decode_batch) {
                        let mut decoded_batch: Vec<(usize, Arc<CpuTile>)> = batch
                            .par_iter()
                            .map(|idx| {
                                let needed = &needed_strips[*idx];
                                let strip = self.get_or_decode_ndpi_strip(
                                    &needed.strip_req,
                                    ifd_id,
                                    jpeg_header,
                                    mcu_starts_tag,
                                    tiles_across,
                                    tiles_down,
                                    strip_offset,
                                    strip_byte_count,
                                    needed.strip_key,
                                    vtw,
                                    vth,
                                    level_w as u32,
                                    level_h as u32,
                                )?;
                                Ok::<(usize, Arc<CpuTile>), WsiError>((*idx, strip))
                            })
                            .collect::<Result<_, _>>()?;
                        decoded.append(&mut decoded_batch);
                    }
                    Ok(decoded)
                };
            for (idx, strip) in decoded_missing? {
                needed_strips[idx].strip = Some(strip);
            }
        }

        let mut tile_data = vec![255u8; (content_width * content_height * 3) as usize];
        let dst_stride = content_width as usize * 3;

        for needed in needed_strips {
            let strip = needed.strip.ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("missing decoded NDPI strip {:?}", needed.strip_key),
            })?;

            let copy_start_x = needed.copy_start_x;
            let copy_start_y = needed.copy_start_y;
            let copy_width = needed.copy_width;
            let copy_height = needed.copy_height;
            let dest_x = needed.dest_x;
            let dest_y = needed.dest_y;

            if strip.layout != CpuTileLayout::Interleaved || strip.channels != 3 {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NDPI display tile expected interleaved RGB strips".into(),
                });
            }
            let CpuTileData::U8(strip_rgb) = &strip.data else {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NDPI display tile expected U8 RGB strip data".into(),
                });
            };
            let src_stride = strip.width as usize * 3;
            let copy_row_bytes = copy_width as usize * 3;
            for row in 0..copy_height as usize {
                let src_off =
                    (copy_start_y as usize + row) * src_stride + copy_start_x as usize * 3;
                let dst_off = (dest_y as usize + row) * dst_stride + dest_x as usize * 3;
                tile_data[dst_off..dst_off + copy_row_bytes]
                    .copy_from_slice(&strip_rgb[src_off..src_off + copy_row_bytes]);
            }
        }

        Ok(CpuTile {
            width: content_width,
            height: content_height,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(tile_data),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn ndpi_jpeg_tile_payload(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        mcu_starts_tag: u16,
        tiles_across: u32,
        tiles_down: u32,
        strip_offset: u64,
        strip_byte_count: u64,
        strip_key: NdpiStripKey,
        virtual_tile_width: u32,
        virtual_tile_height: u32,
        level_width: u32,
        level_height: u32,
    ) -> Result<NdpiJpegTilePayload, WsiError> {
        if strip_key.native_row >= tiles_down {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("NDPI strip row {} out of range", strip_key.native_row),
            });
        }
        if strip_key.col >= tiles_across {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("NDPI strip column {} out of range", strip_key.col),
            });
        }

        let strip_origin_y = strip_key.native_row * virtual_tile_height;
        let strip_height = virtual_tile_height.min(level_height.saturating_sub(strip_origin_y));
        let strip_width =
            virtual_tile_width.min(level_width.saturating_sub(strip_key.col * virtual_tile_width));

        let mcu_starts = self.ndpi_mcu_starts(ifd_id, mcu_starts_tag)?;

        let idx =
            (strip_key.native_row as u64 * tiles_across as u64 + strip_key.col as u64) as usize;
        if idx >= mcu_starts.len() {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "NDPI MCU-starts index {} out of range (len={})",
                    idx,
                    mcu_starts.len(),
                ),
            });
        }

        let file_absolute_mcu_starts = Self::ndpi_mcu_starts_are_file_absolute(
            mcu_starts.as_slice(),
            strip_offset,
            strip_byte_count,
        );

        let segment_start = Self::normalize_ndpi_mcu_start(
            *mcu_starts.get(idx).ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("NDPI MCU-starts index {idx} out of range"),
            })?,
            file_absolute_mcu_starts,
            strip_offset,
        );
        let next_segment_start = if idx + 1 < mcu_starts.len() {
            Some(Self::normalize_ndpi_mcu_start(
                mcu_starts[idx + 1],
                file_absolute_mcu_starts,
                strip_offset,
            ))
        } else {
            None
        };

        if idx + 1 < mcu_starts.len() && next_segment_start <= Some(segment_start) {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "NDPI MCU-starts table is not strictly increasing at index {}",
                    idx
                ),
            });
        }

        let segment_end = next_segment_start.unwrap_or(strip_byte_count);
        if segment_start >= strip_byte_count
            || segment_end > strip_byte_count
            || segment_end <= segment_start
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "NDPI MCU segment [{segment_start}, {segment_end}) exceeds strip byte count {strip_byte_count}"
                ),
            });
        }
        if jpeg_header.is_empty() {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "NDPI JPEG header is empty".into(),
            });
        }

        let segment_len = segment_end.saturating_sub(segment_start);
        let read_offset =
            strip_offset
                .checked_add(segment_start)
                .ok_or_else(|| WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NDPI strip offset overflow".into(),
                })?;
        let segment = self
            .container
            .pread(read_offset, segment_len)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;
        let entropy = strip_trailing_eoi_marker(strip_trailing_restart_marker(
            strip_leading_restart_marker(&segment),
        ));
        let mut tile_jpeg = Vec::with_capacity(jpeg_header.len() + entropy.len() + 2);
        tile_jpeg.extend_from_slice(jpeg_header);
        disable_jpeg_restart_interval(&mut tile_jpeg);
        tile_jpeg.extend_from_slice(entropy);
        tile_jpeg.extend_from_slice(&[0xFF, 0xD9]);

        Ok(NdpiJpegTilePayload {
            jpeg: tile_jpeg,
            width: strip_width,
            height: strip_height,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn decode_ndpi_strip(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        mcu_starts_tag: u16,
        tiles_across: u32,
        tiles_down: u32,
        strip_offset: u64,
        strip_byte_count: u64,
        strip_key: NdpiStripKey,
        virtual_tile_width: u32,
        virtual_tile_height: u32,
        level_width: u32,
        level_height: u32,
    ) -> Result<Arc<CpuTile>, WsiError> {
        let payload = self.ndpi_jpeg_tile_payload(
            req,
            ifd_id,
            jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            strip_offset,
            strip_byte_count,
            strip_key,
            virtual_tile_width,
            virtual_tile_height,
            level_width,
            level_height,
        )?;
        let decoded = decode_jpeg_rgb_with_size_override(
            &payload.jpeg,
            None,
            payload.width,
            payload.height,
            None,
            None,
            self.tiff_jpeg_decode_options_for_data(ifd_id, false, &payload.jpeg, None)
                .color_transform,
        )?;
        let decoded = cpu_tile_from_rgb_pixels(decoded.width, decoded.height, decoded.pixels)?;

        Ok(Arc::new(decoded))
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn ndpi_jpeg_decode_job<'a>(
        &'a self,
        req: &TileRequest,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        mcu_starts_tag: u16,
        tiles_across: u32,
        tiles_down: u32,
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<JpegDecodeJob<'a>, WsiError> {
        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let (level_w, level_h) = level.dimensions;
        let (vtw, vth) = match &level.tile_layout {
            TileLayout::WholeLevel {
                virtual_tile_width,
                virtual_tile_height,
                ..
            } => (*virtual_tile_width, *virtual_tile_height),
            _ => {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NdpiJpeg device decode expects WholeLevel tile layout".into(),
                });
            }
        };
        let (col, row) = validate_tile_coords(req.col, req.row, req.level.get())?;
        let payload = self.ndpi_jpeg_tile_payload(
            req,
            ifd_id,
            jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            strip_offset,
            strip_byte_count,
            NdpiStripKey {
                ifd_id,
                col,
                native_row: row,
            },
            vtw,
            vth,
            level_w as u32,
            level_h as u32,
        )?;
        let color_transform = self
            .tiff_jpeg_decode_options_for_data(ifd_id, false, &payload.jpeg, None)
            .color_transform;
        Ok(JpegDecodeJob {
            data: Cow::Owned(payload.jpeg),
            tables: None,
            expected_width: payload.width,
            expected_height: payload.height,
            color_transform,
            force_dimensions: true,
            requested_size: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn get_or_decode_ndpi_strip(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        mcu_starts_tag: u16,
        tiles_across: u32,
        tiles_down: u32,
        strip_offset: u64,
        strip_byte_count: u64,
        strip_key: NdpiStripKey,
        virtual_tile_width: u32,
        virtual_tile_height: u32,
        level_width: u32,
        level_height: u32,
    ) -> Result<Arc<CpuTile>, WsiError> {
        if let Some(strip) = {
            let mut cache = self
                .ndpi_strip_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.get(&strip_key)
        } {
            return Ok(strip);
        }

        let mut flights = self
            .ndpi_strip_flights
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut registered_waiter = false;
        loop {
            match flights.get_mut(&strip_key) {
                Some(flight) => {
                    if !registered_waiter {
                        flight.waiters += 1;
                        registered_waiter = true;
                    }
                    if let Some(result) = flight.result.clone() {
                        flight.waiters -= 1;
                        if flight.waiters == 0 {
                            flights.remove(&strip_key);
                        }
                        return result.map_err(|reason| Self::ndpi_full_decode_error(req, reason));
                    }
                    flights = self
                        .ndpi_strip_ready
                        .wait(flights)
                        .unwrap_or_else(|e| e.into_inner());
                }
                None if registered_waiter => {
                    return Err(Self::ndpi_full_decode_error(
                        req,
                        format!("NDPI strip decode flight for {:?} disappeared", strip_key),
                    ));
                }
                None => {
                    flights.insert(strip_key, NdpiStripFlight::default());
                    break;
                }
            }
        }
        drop(flights);

        let decode_result = self
            .decode_ndpi_strip(
                req,
                ifd_id,
                jpeg_header,
                mcu_starts_tag,
                tiles_across,
                tiles_down,
                strip_offset,
                strip_byte_count,
                strip_key,
                virtual_tile_width,
                virtual_tile_height,
                level_width,
                level_height,
            )
            .map_err(|err| err.to_string());

        if let Ok(strip) = decode_result.as_ref() {
            let mut cache = self
                .ndpi_strip_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.put(strip_key, strip.clone());
        }

        let mut flights = self
            .ndpi_strip_flights
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(flight) = flights.get_mut(&strip_key) {
            flight.result = Some(decode_result.clone());
            if flight.waiters == 0 {
                flights.remove(&strip_key);
            }
        }
        drop(flights);
        self.ndpi_strip_ready.notify_all();

        decode_result.map_err(|reason| Self::ndpi_full_decode_error(req, reason))
    }

    pub(super) fn get_or_decode_ndpi_full_image(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<Arc<CpuTile>, WsiError> {
        if let Some(img) = {
            let mut cache = self
                .full_decode_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.get(&ifd_id)
        } {
            return Ok(img);
        }

        let mut flights = self
            .full_decode_flights
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut registered_waiter = false;
        loop {
            match flights.get_mut(&ifd_id) {
                Some(flight) => {
                    if !registered_waiter {
                        flight.waiters += 1;
                        registered_waiter = true;
                    }
                    if let Some(result) = flight.result.clone() {
                        flight.waiters -= 1;
                        let should_remove = flight.waiters == 0;
                        if should_remove {
                            flights.remove(&ifd_id);
                        }
                        return result.map_err(|reason| Self::ndpi_full_decode_error(req, reason));
                    }
                    flights = self
                        .full_decode_ready
                        .wait(flights)
                        .unwrap_or_else(|e| e.into_inner());
                }
                None if registered_waiter => {
                    return Err(Self::ndpi_full_decode_error(
                        req,
                        format!("NDPI full decode flight for {ifd_id} disappeared"),
                    ));
                }
                None => {
                    flights.insert(ifd_id, FullDecodeFlight::default());
                    break;
                }
            }
        }
        drop(flights);

        let decode_result = self
            .decode_ndpi_full_image(req, ifd_id, strip_offset, strip_byte_count)
            .map_err(|err| err.to_string());
        if let Ok(image) = decode_result.as_ref() {
            let mut cache = self
                .full_decode_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.put(ifd_id, image.clone());
        }

        let mut flights = self
            .full_decode_flights
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(flight) = flights.get_mut(&ifd_id) {
            flight.result = Some(decode_result.clone());
            if flight.waiters == 0 {
                flights.remove(&ifd_id);
            }
        }
        drop(flights);
        self.full_decode_ready.notify_all();

        decode_result.map_err(|reason| Self::ndpi_full_decode_error(req, reason))
    }

    /// Read a tile from an NdpiJpeg source (MCU extraction fast path).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn read_ndpi_restart_tile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        mcu_starts_tag: u16,
        tiles_across: u32,
        tiles_down: u32,
        _restart_interval: u16,
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<CpuTile, WsiError> {
        let col = req.col;
        let row = req.row;

        // Bounds check
        if col < 0 || col >= tiles_across as i64 || row < 0 || row >= tiles_down as i64 {
            return Err(WsiError::TileRead {
                col,
                row,
                level: req.level.get(),
                reason: format!(
                    "tile ({},{}) out of range ({}x{})",
                    col, row, tiles_across, tiles_down,
                ),
            });
        }

        // Compute tile dimensions first (needed for empty-tile fallback and decode)
        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let (level_w, level_h) = level.dimensions;
        let (vtw, vth) = match &level.tile_layout {
            TileLayout::WholeLevel {
                virtual_tile_width,
                virtual_tile_height,
                ..
            } => (*virtual_tile_width, *virtual_tile_height),
            _ => {
                return Err(WsiError::TileRead {
                    col,
                    row,
                    level: req.level.get(),
                    reason: "NdpiJpeg expects WholeLevel tile layout".into(),
                });
            }
        };

        let strip_key = NdpiStripKey {
            ifd_id,
            col: col as u32,
            native_row: row as u32,
        };
        let strip = self.get_or_decode_ndpi_strip(
            req,
            ifd_id,
            jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            strip_offset,
            strip_byte_count,
            strip_key,
            vtw,
            vth,
            level_w as u32,
            level_h as u32,
        )?;

        Ok(strip.as_ref().clone())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn read_ndpi_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        mcu_starts_tag: u16,
        tiles_across: u32,
        tiles_down: u32,
        restart_interval: u16,
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<RawCompressedTile, WsiError> {
        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let (level_w, level_h) = level.dimensions;
        let (virtual_tile_width, virtual_tile_height) = match level.tile_layout {
            TileLayout::WholeLevel {
                virtual_tile_width,
                virtual_tile_height,
                ..
            } if virtual_tile_width > 0 && virtual_tile_height > 0 => {
                (virtual_tile_width, virtual_tile_height)
            }
            TileLayout::WholeLevel { .. } => {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason:
                        "NDPI raw JPEG retile requires nonzero WholeLevel virtual tile dimensions"
                            .into(),
                });
            }
            _ => {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NDPI raw JPEG retile expects WholeLevel tile layout".into(),
                });
            }
        };
        if !ndpi_restart_segments_align_to_rows(level_w, virtual_tile_width, restart_interval) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "NDPI raw JPEG retile requires restart segments to align to image rows (level width {level_w}, virtual tile width {virtual_tile_width}, restart interval {restart_interval})"
                ),
            });
        }

        let tile_origin_x = req
            .col
            .checked_mul(i64::from(req.tile_width))
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "NDPI raw JPEG retile tile x offset overflow".into(),
            })?;
        let tile_origin_y = req
            .row
            .checked_mul(i64::from(req.tile_height))
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "NDPI raw JPEG retile tile y offset overflow".into(),
            })?;
        if tile_origin_x < 0
            || tile_origin_y < 0
            || tile_origin_x >= level_w as i64
            || tile_origin_y >= level_h as i64
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "NDPI raw JPEG retile origin out of bounds".into(),
            });
        }
        let content_width = req.tile_width.min((level_w as i64 - tile_origin_x) as u32);
        let content_height = req.tile_height.min((level_h as i64 - tile_origin_y) as u32);
        if content_width == 0 || content_height == 0 {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "NDPI raw JPEG retile requested empty frame".into(),
            });
        }

        let mut segments = Vec::new();
        let tile_end_x = tile_origin_x as u32 + content_width;
        let tile_end_y = tile_origin_y as u32 + content_height;
        let native_col_start = tile_origin_x as u32 / virtual_tile_width;
        let native_col_end = tile_end_x.saturating_sub(1) / virtual_tile_width;
        let native_row_start = tile_origin_y as u32 / virtual_tile_height;
        let native_row_end = tile_end_y.saturating_sub(1) / virtual_tile_height;

        for native_row in native_row_start..=native_row_end {
            for native_col in native_col_start..=native_col_end {
                let strip_origin_x = native_col * virtual_tile_width;
                let crop_start_x = (tile_origin_x as u32).saturating_sub(strip_origin_x);
                let crop_end_x = tile_end_x.min(strip_origin_x + virtual_tile_width);
                let crop_width = crop_end_x.saturating_sub(strip_origin_x + crop_start_x);
                if crop_width == 0 {
                    continue;
                }
                let strip_req = TileRequest {
                    scene: req.scene.get().into(),
                    series: req.series.get().into(),
                    level: req.level.get().into(),
                    plane: req.plane,
                    col: i64::from(native_col),
                    row: i64::from(native_row),
                };
                let raw = self.read_ndpi_raw_jpeg_tile(
                    &strip_req,
                    ifd_id,
                    jpeg_header,
                    mcu_starts_tag,
                    tiles_across,
                    tiles_down,
                    restart_interval,
                    strip_offset,
                    strip_byte_count,
                )?;
                let image = extract_dct_blocks(raw.data(), DctExtractOptions::default()).map_err(
                    |err| WsiError::Jpeg(format!("NDPI raw JPEG retile DCT extract failed: {err}")),
                )?;
                let mcu_width = image
                    .components
                    .iter()
                    .map(|component| component.h_samp)
                    .max()
                    .unwrap_or(1) as u32
                    * 8;
                let mcu_height = image
                    .components
                    .iter()
                    .map(|component| component.v_samp)
                    .max()
                    .unwrap_or(1) as u32
                    * 8;
                if mcu_width == 0 || mcu_height == 0 {
                    return Err(WsiError::Unsupported {
                        reason: "NDPI raw JPEG retile requires nonzero MCU dimensions".into(),
                    });
                }
                if !(tile_origin_x as u32).is_multiple_of(mcu_width)
                    || !(tile_origin_y as u32).is_multiple_of(mcu_height)
                    || !crop_start_x.is_multiple_of(mcu_width)
                {
                    return Err(WsiError::Unsupported {
                        reason: format!(
                            "NDPI raw JPEG retile requires MCU-aligned frame origins (mcu {}x{})",
                            mcu_width, mcu_height
                        ),
                    });
                }
                segments.push(NdpiDctRetileSegment {
                    native_row,
                    crop_start_mcu: crop_start_x / mcu_width,
                    crop_mcus: crop_width.div_ceil(mcu_width),
                    image,
                });
            }
        }

        let image = build_ndpi_retiled_dct_image(
            req,
            content_width,
            content_height,
            native_col_start,
            native_col_end,
            native_row_start,
            native_row_end,
            &segments,
        )?;
        let data = encode_baseline_dct_image(&image)
            .map_err(|err| WsiError::Jpeg(format!("NDPI raw JPEG retile encode failed: {err}")))?;
        let info = parse_baseline_jpeg_frame_info(&data)?;
        Ok(RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(info.width, info.height)
            .bits_allocated(info.bits_allocated)
            .samples_per_pixel(info.samples_per_pixel)
            .photometric_interpretation(info.photometric_interpretation)
            .data(data)
            .build()?)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn read_ndpi_raw_jpeg_tile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        jpeg_header: &[u8],
        mcu_starts_tag: u16,
        tiles_across: u32,
        tiles_down: u32,
        restart_interval: u16,
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<RawCompressedTile, WsiError> {
        let (col, row) = validate_tile_coords(req.col, req.row, req.level.get())?;
        if col >= tiles_across || row >= tiles_down {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "NDPI raw JPEG tile ({},{}) out of range ({}x{})",
                    req.col, req.row, tiles_across, tiles_down
                ),
            });
        }

        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let (level_w, level_h) = level.dimensions;
        let (virtual_tile_width, virtual_tile_height) = match level.tile_layout {
            TileLayout::WholeLevel {
                virtual_tile_width,
                virtual_tile_height,
                ..
            } if virtual_tile_width > 0 && virtual_tile_height > 0 => {
                (virtual_tile_width, virtual_tile_height)
            }
            TileLayout::WholeLevel { .. } => {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NDPI raw JPEG passthrough requires nonzero WholeLevel virtual tile dimensions"
                        .into(),
                });
            }
            _ => {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NDPI raw JPEG passthrough expects WholeLevel tile layout".into(),
                });
            }
        };
        if !ndpi_restart_segments_align_to_rows(level_w, virtual_tile_width, restart_interval) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "NDPI raw JPEG passthrough requires restart segments to align to image rows (level width {level_w}, virtual tile width {virtual_tile_width}, restart interval {restart_interval})"
                ),
            });
        }

        let payload = self.ndpi_jpeg_tile_payload(
            req,
            ifd_id,
            jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            strip_offset,
            strip_byte_count,
            NdpiStripKey {
                ifd_id,
                col,
                native_row: row,
            },
            virtual_tile_width,
            virtual_tile_height,
            u32::try_from(level_w).map_err(|_| WsiError::Unsupported {
                reason: "NDPI raw JPEG passthrough requires level width to fit in u32".into(),
            })?,
            u32::try_from(level_h).map_err(|_| WsiError::Unsupported {
                reason: "NDPI raw JPEG passthrough requires level height to fit in u32".into(),
            })?,
        )?;
        let mut data = payload.jpeg;
        patch_jpeg_sof0_dimensions(&mut data, virtual_tile_width, virtual_tile_height)?;
        let info = parse_baseline_jpeg_frame_info(&data)?;
        if info.width != virtual_tile_width || info.height != virtual_tile_height {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "NDPI raw JPEG passthrough SOF dimensions {}x{} do not match virtual tile {}x{}",
                    info.width, info.height, virtual_tile_width, virtual_tile_height
                ),
            });
        }

        Ok(RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(info.width, info.height)
            .bits_allocated(info.bits_allocated)
            .samples_per_pixel(info.samples_per_pixel)
            .photometric_interpretation(info.photometric_interpretation)
            .data(data)
            .build()?)
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    pub(super) fn ndpi_jpeg_batchable(&self, reqs: &[TileRequest]) -> Result<bool, WsiError> {
        if reqs.is_empty() {
            return Ok(false);
        }
        for req in reqs {
            if !matches!(self.tile_source_for(req)?, TileSource::NdpiJpeg { .. }) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    pub(super) fn decode_ndpi_jpeg_pixels(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
        require_device: bool,
        metal_sessions: MetalBackendSessionsRef<'_>,
        cuda_sessions: CudaBackendSessionsRef<'_>,
    ) -> Result<Vec<TilePixels>, WsiError> {
        let mut jobs = Vec::with_capacity(reqs.len());
        for req in reqs {
            let source = self.tile_source_for(req)?;
            let TileSource::NdpiJpeg {
                ifd_id,
                jpeg_header,
                mcu_starts_tag,
                tiles_across,
                tiles_down,
                strip_offset,
                strip_byte_count,
                ..
            } = source
            else {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NDPI JPEG device batch received a non-NDPI tile source".into(),
                });
            };
            jobs.push(self.ndpi_jpeg_decode_job(
                req,
                *ifd_id,
                jpeg_header,
                *mcu_starts_tag,
                *tiles_across,
                *tiles_down,
                *strip_offset,
                *strip_byte_count,
            )?);
        }
        decode_batch_jpeg_pixels(
            &jobs,
            backend,
            require_device,
            metal_sessions,
            cuda_sessions,
        )
        .into_iter()
        .zip(reqs.iter())
        .map(|(result, req)| {
            result.map_err(|err| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: err.to_string(),
            })
        })
        .collect()
    }

    /// Read a tile from an NdpiFullDecode source (full JPEG decode fallback).
    ///
    /// Decodes the entire JPEG strip and extracts the requested tile region.
    /// Caches the decoded image in FullDecodeCache for subsequent tile requests
    /// from the same level. Oversize images (larger than cache max) are decoded
    /// per-request without caching.
    pub(super) fn read_ndpi_full_decode_tile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        _jpeg_header: &[u8],
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<CpuTile, WsiError> {
        let full_image =
            self.get_or_decode_ndpi_full_image(req, ifd_id, strip_offset, strip_byte_count)?;

        // Extract the requested tile from the full image
        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let (level_w, level_h) = level.dimensions;
        let (vtw, vth) = match &level.tile_layout {
            TileLayout::WholeLevel {
                virtual_tile_width,
                virtual_tile_height,
                ..
            } => (*virtual_tile_width, *virtual_tile_height),
            _ => {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "NdpiFullDecode expects WholeLevel tile layout".into(),
                });
            }
        };

        let (col_u32, row_u32) = validate_tile_coords(req.col, req.row, req.level.get())?;
        let src_x = col_u32 * vtw;
        let src_y = row_u32 * vth;
        let tile_w = vtw.min((level_w as u32).saturating_sub(src_x));
        let tile_h = vth.min((level_h as u32).saturating_sub(src_y));

        if tile_w == 0 || tile_h == 0 {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "tile has zero dimensions".into(),
            });
        }

        // Extract sub-region from the full interleaved RGB image
        let full_w = full_image.width as usize;
        let channels = full_image.channels as usize;
        let src_data = full_image.data.as_u8().ok_or_else(|| WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "expected U8 data in full decode cache".into(),
        })?;

        let mut tile_data = Vec::with_capacity(tile_w as usize * tile_h as usize * channels);
        for y in 0..tile_h {
            let row_start = ((src_y + y) as usize * full_w + src_x as usize) * channels;
            let row_end = row_start + tile_w as usize * channels;
            if row_end > src_data.len() {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "decoded image smaller than expected".into(),
                });
            }
            tile_data.extend_from_slice(&src_data[row_start..row_end]);
        }

        Ok(CpuTile {
            width: tile_w,
            height: tile_h,
            channels: full_image.channels,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(tile_data),
        })
    }

    pub(super) fn read_ndpi_full_display_tile(
        &self,
        req: &TileViewRequest,
        ifd_id: IfdId,
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<CpuTile, WsiError> {
        let tile_req = TileRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: req.level.get().into(),
            plane: req.plane,
            col: req.col,
            row: req.row,
        };
        let full_image =
            self.get_or_decode_ndpi_full_image(&tile_req, ifd_id, strip_offset, strip_byte_count)?;
        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let (level_w, level_h) = (level.dimensions.0 as i64, level.dimensions.1 as i64);
        let tile_origin_x = req.col.saturating_mul(i64::from(req.tile_width));
        let tile_origin_y = req.row.saturating_mul(i64::from(req.tile_height));
        if tile_origin_x < 0
            || tile_origin_y < 0
            || tile_origin_x >= level_w
            || tile_origin_y >= level_h
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "display tile origin out of bounds".into(),
            });
        }

        let tile_w = req.tile_width.min((level_w - tile_origin_x) as u32);
        let tile_h = req.tile_height.min((level_h - tile_origin_y) as u32);
        let full_w = full_image.width as usize;
        let channels = full_image.channels as usize;
        let src_data = full_image.data.as_u8().ok_or_else(|| WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "expected U8 data in full decode cache".into(),
        })?;

        let mut tile_data = Vec::with_capacity(tile_w as usize * tile_h as usize * channels);
        for y in 0..tile_h {
            let row_start =
                ((tile_origin_y as u32 + y) as usize * full_w + tile_origin_x as usize) * channels;
            let row_end = row_start + tile_w as usize * channels;
            if row_end > src_data.len() {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "decoded image smaller than expected".into(),
                });
            }
            tile_data.extend_from_slice(&src_data[row_start..row_end]);
        }

        Ok(CpuTile {
            width: tile_w,
            height: tile_h,
            channels: full_image.channels,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(tile_data),
        })
    }
}

struct NdpiDctRetileSegment {
    native_row: u32,
    crop_start_mcu: u32,
    crop_mcus: u32,
    image: JpegDctImage,
}

#[allow(clippy::too_many_arguments)]
fn build_ndpi_retiled_dct_image(
    req: &TileViewRequest,
    content_width: u32,
    content_height: u32,
    native_col_start: u32,
    native_col_end: u32,
    native_row_start: u32,
    native_row_end: u32,
    segments: &[NdpiDctRetileSegment],
) -> Result<JpegDctImage, WsiError> {
    let Some(first) = segments.first().map(|segment| &segment.image) else {
        return Err(WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "NDPI raw JPEG retile found no source segments".into(),
        });
    };
    if first.coding_mode != JpegDctCodingMode::BaselineSequential {
        return Err(WsiError::Unsupported {
            reason: "NDPI raw JPEG retile supports baseline sequential JPEG only".into(),
        });
    }
    let component_count = first.components.len();
    if component_count != 1 && component_count != 3 {
        return Err(WsiError::Unsupported {
            reason: format!(
                "NDPI raw JPEG retile supports 1 or 3 components, got {component_count}"
            ),
        });
    }
    let max_h = first
        .components
        .iter()
        .map(|component| component.h_samp)
        .max()
        .unwrap_or(1);
    let max_v = first
        .components
        .iter()
        .map(|component| component.v_samp)
        .max()
        .unwrap_or(1);
    let mcu_width = u32::from(max_h) * 8;
    let mcu_height = u32::from(max_v) * 8;
    if mcu_width == 0 || mcu_height == 0 {
        return Err(WsiError::Unsupported {
            reason: "NDPI raw JPEG retile requires nonzero MCU dimensions".into(),
        });
    }
    let output_mcu_cols = content_width.div_ceil(mcu_width);
    let output_mcu_rows = content_height.div_ceil(mcu_height);
    let expected_segments_per_row = native_col_end - native_col_start + 1;
    let mut component_blocks = (0..component_count)
        .map(|idx| {
            let component = &first.components[idx];
            let capacity = output_mcu_cols
                .saturating_mul(output_mcu_rows)
                .saturating_mul(u32::from(component.h_samp))
                .saturating_mul(u32::from(component.v_samp));
            Vec::with_capacity(capacity as usize)
        })
        .collect::<Vec<_>>();

    for native_row in native_row_start..=native_row_end {
        let row_segments = segments
            .iter()
            .filter(|segment| segment.native_row == native_row)
            .collect::<Vec<_>>();
        if row_segments.len() != expected_segments_per_row as usize {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "NDPI raw JPEG retile row {native_row} has {} source segments, expected {expected_segments_per_row}",
                    row_segments.len()
                ),
            });
        }

        for (component_idx, blocks) in component_blocks
            .iter_mut()
            .enumerate()
            .take(component_count)
        {
            let reference = &first.components[component_idx];
            let h = u32::from(reference.h_samp);
            let v = u32::from(reference.v_samp);
            for block_y in 0..v {
                let row_start_len = blocks.len();
                for segment in &row_segments {
                    validate_ndpi_retile_segment(first, &segment.image)?;
                    let component = &segment.image.components[component_idx];
                    if component.block_rows != v {
                        return Err(WsiError::Unsupported {
                            reason: format!(
                                "NDPI raw JPEG retile expects one MCU row per source segment, got {} block rows for component {component_idx}",
                                component.block_rows
                            ),
                        });
                    }
                    let start_block = block_y * component.block_cols + segment.crop_start_mcu * h;
                    let block_count = segment.crop_mcus * h;
                    let end_block = start_block.checked_add(block_count).ok_or_else(|| {
                        WsiError::Unsupported {
                            reason: "NDPI raw JPEG retile block range overflow".into(),
                        }
                    })?;
                    let start = start_block as usize;
                    let end = end_block as usize;
                    if end > component.quantized_blocks.len() {
                        return Err(WsiError::Unsupported {
                            reason: format!(
                                "NDPI raw JPEG retile crop exceeds component {component_idx} block grid"
                            ),
                        });
                    }
                    blocks.extend_from_slice(&component.quantized_blocks[start..end]);
                }
                let copied = blocks.len() - row_start_len;
                let expected = (output_mcu_cols * h) as usize;
                if copied != expected {
                    return Err(WsiError::Unsupported {
                        reason: format!(
                            "NDPI raw JPEG retile copied {copied} blocks for component {component_idx}, expected {expected}"
                        ),
                    });
                }
            }
        }
    }

    let components = first
        .components
        .iter()
        .enumerate()
        .map(|(idx, component)| JpegDctComponent {
            component_index: idx,
            width: content_width
                .saturating_mul(u32::from(component.h_samp))
                .div_ceil(u32::from(max_h)),
            height: content_height
                .saturating_mul(u32::from(component.v_samp))
                .div_ceil(u32::from(max_v)),
            h_samp: component.h_samp,
            v_samp: component.v_samp,
            block_cols: output_mcu_cols * u32::from(component.h_samp),
            block_rows: output_mcu_rows * u32::from(component.v_samp),
            quant_table: component.quant_table,
            quantized_blocks: std::mem::take(&mut component_blocks[idx]),
            dequantized_blocks: Vec::new(),
        })
        .collect();

    Ok(JpegDctImage {
        width: content_width,
        height: content_height,
        color_space: first.color_space,
        coding_mode: JpegDctCodingMode::BaselineSequential,
        scan_count: 1,
        components,
        restart_index: None,
    })
}

fn validate_ndpi_retile_segment(
    reference: &JpegDctImage,
    candidate: &JpegDctImage,
) -> Result<(), WsiError> {
    if candidate.coding_mode != JpegDctCodingMode::BaselineSequential {
        return Err(WsiError::Unsupported {
            reason: "NDPI raw JPEG retile supports baseline sequential JPEG only".into(),
        });
    }
    if candidate.color_space != reference.color_space
        || candidate.components.len() != reference.components.len()
    {
        return Err(WsiError::Unsupported {
            reason: "NDPI raw JPEG retile source segment color profile changed".into(),
        });
    }
    for (idx, (expected, actual)) in reference
        .components
        .iter()
        .zip(candidate.components.iter())
        .enumerate()
    {
        if expected.h_samp != actual.h_samp
            || expected.v_samp != actual.v_samp
            || expected.quant_table != actual.quant_table
        {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "NDPI raw JPEG retile source segment component {idx} coding profile changed"
                ),
            });
        }
    }
    Ok(())
}
