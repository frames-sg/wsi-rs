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
        let options = j2k_decode_options(
            self.tiff_jpeg_decode_options_for_data(ifd_id, false, &data, None)
                .color_transform,
        );
        let view = J2kJpegView::parse_with_options(&data, options)
            .map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let decoder =
            J2kJpegDecoder::from_view(view).map_err(|err| WsiError::Jpeg(err.to_string()))?;
        let (pixels, outcome) = decoder
            .decode_request(J2kJpegDecodeRequest::full(J2kPixelFormat::Rgb8))
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
}
