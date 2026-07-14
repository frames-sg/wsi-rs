use super::*;

impl TiffPixelReader {
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
    fn crop_ndpi_full_image_tile(
        full_image: &CpuTile,
        req: &TileRequest,
        src_x: u32,
        src_y: u32,
        tile_w: u32,
        tile_h: u32,
    ) -> Result<CpuTile, WsiError> {
        let full_w = full_image.width as usize;
        let channels = full_image.channels as usize;
        let src_data = full_image.data.as_u8().ok_or_else(|| WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "expected U8 data in full decode cache".into(),
        })?;

        let tile_len = crate::core::limits::checked_product_to_usize(
            &[u64::from(tile_w), u64::from(tile_h), channels as u64],
            crate::core::limits::MAX_DECODED_IMAGE_BYTES,
            "NDPI decoded tile",
        )
        .map_err(|reason| WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason,
        })?;
        let mut tile_data = Vec::with_capacity(tile_len);
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
        if tile_data.len() != tile_len {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "decoded NDPI tile length mismatch".into(),
            });
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

        Self::crop_ndpi_full_image_tile(full_image.as_ref(), req, src_x, src_y, tile_w, tile_h)
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
        Self::crop_ndpi_full_image_tile(
            full_image.as_ref(),
            &tile_req,
            tile_origin_x as u32,
            tile_origin_y as u32,
            tile_w,
            tile_h,
        )
    }
}
