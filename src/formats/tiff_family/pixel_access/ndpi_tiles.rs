use super::ndpi_core::ndpi_restart_segments_align_to_rows;
use super::*;

/// A tile read through its NDPI restart-marker segment.
#[derive(Debug)]
pub(super) enum NdpiRestartTile {
    Decoded(CpuTile),
    /// The MCU-starts table cannot locate the tile's restart segment; only a
    /// full strip decode can serve it.
    NeedsFullDecode,
}

impl TiffPixelReader {
    pub(super) fn get_or_decode_ndpi_full_image(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        strip_offset: u64,
        strip_byte_count: u64,
    ) -> Result<Arc<CpuTile>, WsiError> {
        self.full_decode_cache
            .get_or_try_insert_with(FullDecodeKey::Level(ifd_id), || {
                self.decode_ndpi_full_image(req, ifd_id, strip_offset, strip_byte_count)
                    .map_err(|err| err.to_string())
            })
            .map_err(|reason| WsiError::tile_read(req, reason))
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
    ) -> Result<NdpiRestartTile, WsiError> {
        let col = req.col;
        let row = req.row;

        checked_grid_tile(req, u64::from(tiles_across), u64::from(tiles_down))?;

        // Compute tile dimensions first (needed for empty-tile fallback and decode)
        let (level_w, level_h, vtw, vth) = self.ndpi_whole_level_geometry(
            req.scene,
            req.series,
            req.level,
            req.col,
            req.row,
            "NdpiJpeg expects WholeLevel tile layout",
        )?;

        let strip_key = NdpiStripKey {
            ifd_id,
            col: col as u32,
            native_row: row as u32,
            scale_denom: 1,
        };
        let strip = match self.get_or_decode_ndpi_strip(
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
        ) {
            Ok(strip) => strip,
            // Checked only after a failed decode, so cached strips skip the
            // table lookup. The decode runs the same lookup before reading the
            // strip, so an unlocatable segment is what failed it.
            Err(_)
                if self.ndpi_restart_segment_is_unlocatable(
                    ifd_id,
                    mcu_starts_tag,
                    tiles_across,
                    strip_offset,
                    strip_byte_count,
                    strip_key,
                ) =>
            {
                return Ok(NdpiRestartTile::NeedsFullDecode);
            }
            Err(err) => return Err(err),
        };

        Ok(NdpiRestartTile::Decoded(strip.as_ref().clone()))
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
        let (level_w, level_h, virtual_tile_width, virtual_tile_height) = self
            .ndpi_whole_level_geometry(
                req.scene,
                req.series,
                req.level,
                req.col,
                req.row,
                "NDPI raw JPEG retile expects WholeLevel tile layout",
            )?;
        if virtual_tile_width == 0 || virtual_tile_height == 0 {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "NDPI raw JPEG retile requires nonzero WholeLevel virtual tile dimensions"
                    .into(),
            });
        }
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
                    col: i64::from(native_col),
                    row: i64::from(native_row),
                    ..req.source_tile()
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
        let mut info = parse_baseline_jpeg_frame_info(&data)?;
        if image.color_space == j2k_jpeg::ColorSpace::Rgb {
            info.photometric_interpretation = EncodedTilePhotometricInterpretation::Rgb;
        }
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
            return Err(WsiError::tile_read(
                req,
                format!(
                    "NDPI raw JPEG tile ({},{}) out of range ({}x{})",
                    req.col, req.row, tiles_across, tiles_down
                ),
            ));
        }

        let (level_w, level_h, virtual_tile_width, virtual_tile_height) = self
            .ndpi_whole_level_geometry(
                req.scene,
                req.series,
                req.level,
                req.col,
                req.row,
                "NDPI raw JPEG passthrough expects WholeLevel tile layout",
            )?;
        if virtual_tile_width == 0 || virtual_tile_height == 0 {
            return Err(WsiError::tile_read(
                req,
                "NDPI raw JPEG passthrough requires nonzero WholeLevel virtual tile dimensions",
            ));
        }
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
                scale_denom: 1,
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
}
