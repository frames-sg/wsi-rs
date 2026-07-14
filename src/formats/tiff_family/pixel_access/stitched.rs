use super::*;

impl TiffPixelReader {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn get_or_decode_stitched_component_tile(
        &self,
        ifd_id: IfdId,
        tile_idx: usize,
        jpeg_tables: Option<&[u8]>,
        compression: Compression,
        width: u32,
        height: u32,
        offsets: &[u64],
        byte_counts: &[u64],
    ) -> Result<Arc<CpuTile>, WsiError> {
        let key = StitchedComponentTileKey {
            ifd_id,
            tile_idx,
            width,
            height,
        };
        if let Some(cached) = self
            .stitched_component_tile_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Ok(cached);
        }

        let tile = Arc::new(self.decode_tiled_ifd_tile_index(
            ifd_id,
            tile_idx,
            jpeg_tables,
            compression,
            width,
            height,
            offsets,
            byte_counts,
            BackendRequest::Auto,
        )?);
        self.stitched_component_tile_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(key, tile.clone());
        Ok(tile)
    }

    pub(super) fn read_stitched_level_tile(
        &self,
        req: &TileRequest,
        components: &[StitchedLevelComponent],
        direct_tiles: &HashMap<(i64, i64), usize>,
    ) -> Result<CpuTile, WsiError> {
        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];
        let TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } = &level.tile_layout
        else {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "stitched level expects regular public tile layout".into(),
            });
        };

        if req.col < 0
            || req.row < 0
            || req.col >= *tiles_across as i64
            || req.row >= *tiles_down as i64
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "tile ({},{}) out of range ({}x{})",
                    req.col, req.row, tiles_across, tiles_down
                ),
            });
        }

        let public_x = req.col * i64::from(*tile_width);
        let public_y = req.row * i64::from(*tile_height);
        let out_width = (*tile_width)
            .min((level.dimensions.0 as u32).saturating_sub(req.col as u32 * *tile_width));
        let out_height = (*tile_height)
            .min((level.dimensions.1 as u32).saturating_sub(req.row as u32 * *tile_height));

        if let Some(tile) = self.try_read_stitched_level_direct_tile(
            req.col,
            req.row,
            public_x,
            public_y,
            out_width,
            out_height,
            components,
            direct_tiles,
        )? {
            return Ok(tile);
        }

        let out_len = checked_product_to_usize(
            &[u64::from(out_width), u64::from(out_height), 3],
            MAX_DECODED_IMAGE_BYTES,
            "stitched TIFF tile",
        )
        .map_err(WsiError::DisplayConversion)?;
        let mut out = vec![0u8; out_len];
        let out_stride = out_width as usize * 3;

        for component in components {
            let comp_left = component.origin_x;
            let comp_top = component.origin_y;
            let comp_right = comp_left + component.width as i64;
            let comp_bottom = comp_top + component.height as i64;
            let tile_right = public_x + i64::from(out_width);
            let tile_bottom = public_y + i64::from(out_height);

            let inter_left = public_x.max(comp_left);
            let inter_top = public_y.max(comp_top);
            let inter_right = tile_right.min(comp_right);
            let inter_bottom = tile_bottom.min(comp_bottom);
            if inter_left >= inter_right || inter_top >= inter_bottom {
                continue;
            }

            let local_x = (inter_left - comp_left) as u32;
            let local_y = (inter_top - comp_top) as u32;
            let inter_width = (inter_right - inter_left) as u32;
            let inter_height = (inter_bottom - inter_top) as u32;
            let region = self.read_tiled_ifd_component_region(
                component,
                local_x,
                local_y,
                inter_width,
                inter_height,
            )?;

            let region_data = region.data.as_u8().ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "stitched component produced non-U8 data".into(),
            })?;
            let dst_x = (inter_left - public_x) as usize;
            let dst_y = (inter_top - public_y) as usize;
            let src_stride = inter_width as usize * 3;
            for row in 0..inter_height as usize {
                let src_off = row * src_stride;
                let dst_off = (dst_y + row) * out_stride + dst_x * 3;
                out[dst_off..dst_off + src_stride]
                    .copy_from_slice(&region_data[src_off..src_off + src_stride]);
            }
        }

        Ok(CpuTile {
            width: out_width,
            height: out_height,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(out),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_read_stitched_level_direct_tile(
        &self,
        public_col: i64,
        public_row: i64,
        public_x: i64,
        public_y: i64,
        out_width: u32,
        out_height: u32,
        components: &[StitchedLevelComponent],
        direct_tiles: &HashMap<(i64, i64), usize>,
    ) -> Result<Option<CpuTile>, WsiError> {
        let tile_right = public_x + i64::from(out_width);
        let tile_bottom = public_y + i64::from(out_height);
        let component = if let Some(&index) = direct_tiles.get(&(public_col, public_row)) {
            components.get(index)
        } else {
            let mut covering_component: Option<&StitchedLevelComponent> = None;

            for component in components {
                let comp_left = component.origin_x;
                let comp_top = component.origin_y;
                let comp_right = comp_left + component.width as i64;
                let comp_bottom = comp_top + component.height as i64;

                let inter_left = public_x.max(comp_left);
                let inter_top = public_y.max(comp_top);
                let inter_right = tile_right.min(comp_right);
                let inter_bottom = tile_bottom.min(comp_bottom);
                if inter_left >= inter_right || inter_top >= inter_bottom {
                    continue;
                }
                if inter_left != public_x
                    || inter_top != public_y
                    || inter_right != tile_right
                    || inter_bottom != tile_bottom
                {
                    return Ok(None);
                }
                if covering_component.is_some() {
                    return Ok(None);
                }
                covering_component = Some(component);
            }
            covering_component
        };

        let Some(component) = component else {
            return Ok(None);
        };

        let local_x = (public_x - component.origin_x) as u32;
        let local_y = (public_y - component.origin_y) as u32;
        let tile_col = local_x / component.tile_width;
        let tile_row = local_y / component.tile_height;
        if u64::from(tile_col) >= component.tiles_across
            || u64::from(tile_row) >= component.tiles_down
        {
            return Ok(None);
        }

        let tile_left = tile_col.saturating_mul(component.tile_width);
        let tile_top = tile_row.saturating_mul(component.tile_height);
        let decoded_width = component.tile_width.min(
            (component.width as u32).saturating_sub(tile_col.saturating_mul(component.tile_width)),
        );
        let decoded_height = component.tile_height.min(
            (component.height as u32)
                .saturating_sub(tile_row.saturating_mul(component.tile_height)),
        );
        if local_x < tile_left
            || local_y < tile_top
            || local_x.saturating_add(out_width) > tile_left.saturating_add(decoded_width)
            || local_y.saturating_add(out_height) > tile_top.saturating_add(decoded_height)
        {
            return Ok(None);
        }

        let tile_idx =
            (u64::from(tile_row) * component.tiles_across + u64::from(tile_col)) as usize;
        let (offsets, byte_counts) = self.tiled_ifd_offsets_and_byte_counts(component.ifd_id)?;
        let tile = self.get_or_decode_stitched_component_tile(
            component.ifd_id,
            tile_idx,
            component.jpeg_tables.as_deref(),
            component.compression,
            decoded_width,
            decoded_height,
            offsets,
            byte_counts,
        )?;
        let crop_x = local_x - tile_left;
        let crop_y = local_y - tile_top;
        if crop_x == 0 && crop_y == 0 && decoded_width == out_width && decoded_height == out_height
        {
            Ok(Some(tile.as_ref().clone()))
        } else {
            Ok(Some(crop_rgb_interleaved_u8_buffer(
                tile.as_ref(),
                crop_x,
                crop_y,
                out_width,
                out_height,
            )?))
        }
    }

    pub(super) fn read_tiled_ifd_component_region(
        &self,
        component: &StitchedLevelComponent,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<CpuTile, WsiError> {
        #[derive(Clone, Copy)]
        struct ComponentTileJob {
            decoded_width: u32,
            decoded_height: u32,
            tile_origin_x: u32,
            tile_origin_y: u32,
            inter_left: u32,
            inter_top: u32,
            inter_right: u32,
            inter_bottom: u32,
        }

        let offsets = self
            .container
            .get_u64_array(component.ifd_id, tags::TILE_OFFSETS)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;
        let byte_counts = self
            .container
            .get_u64_array(component.ifd_id, tags::TILE_BYTE_COUNTS)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;

        let out_len = checked_product_to_usize(
            &[u64::from(width), u64::from(height), 3],
            MAX_DECODED_IMAGE_BYTES,
            "stitched TIFF region",
        )
        .map_err(WsiError::DisplayConversion)?;
        let mut out = vec![0u8; out_len];
        let out_stride = width as usize * 3;
        let tile_width = component.tile_width;
        let tile_height = component.tile_height;
        let start_col = x / tile_width;
        let end_col = (x + width - 1) / tile_width;
        let start_row = y / tile_height;
        let end_row = (y + height - 1) / tile_height;
        let mut tile_jobs = Vec::with_capacity(
            ((end_col - start_col + 1) as usize).saturating_mul((end_row - start_row + 1) as usize),
        );

        for row in start_row..=end_row {
            for col in start_col..=end_col {
                if u64::from(col) >= component.tiles_across
                    || u64::from(row) >= component.tiles_down
                {
                    continue;
                }

                let decoded_width = tile_width
                    .min((component.width as u32).saturating_sub(col.saturating_mul(tile_width)));
                let decoded_height = tile_height
                    .min((component.height as u32).saturating_sub(row.saturating_mul(tile_height)));
                let tile_origin_x = col * tile_width;
                let tile_origin_y = row * tile_height;
                let inter_left = x.max(tile_origin_x);
                let inter_top = y.max(tile_origin_y);
                let inter_right = (x + width).min(tile_origin_x + decoded_width);
                let inter_bottom = (y + height).min(tile_origin_y + decoded_height);
                if inter_left >= inter_right || inter_top >= inter_bottom {
                    continue;
                }

                tile_jobs.push(ComponentTileJob {
                    decoded_width,
                    decoded_height,
                    tile_origin_x,
                    tile_origin_y,
                    inter_left,
                    inter_top,
                    inter_right,
                    inter_bottom,
                });
            }
        }

        let decoded_tiles: Vec<_> = if tile_jobs.len() <= 1 {
            tile_jobs
                .into_iter()
                .map(|job| {
                    let tile_idx = (u64::from(job.tile_origin_y / tile_height)
                        * component.tiles_across
                        + u64::from(job.tile_origin_x / tile_width))
                        as usize;
                    let tile = self.get_or_decode_stitched_component_tile(
                        component.ifd_id,
                        tile_idx,
                        component.jpeg_tables.as_deref(),
                        component.compression,
                        job.decoded_width,
                        job.decoded_height,
                        offsets,
                        byte_counts,
                    )?;
                    Ok((job, tile))
                })
                .collect::<Result<_, WsiError>>()?
        } else {
            tile_jobs
                .into_par_iter()
                .map(|job| {
                    let tile_idx = (u64::from(job.tile_origin_y / tile_height)
                        * component.tiles_across
                        + u64::from(job.tile_origin_x / tile_width))
                        as usize;
                    let tile = self.get_or_decode_stitched_component_tile(
                        component.ifd_id,
                        tile_idx,
                        component.jpeg_tables.as_deref(),
                        component.compression,
                        job.decoded_width,
                        job.decoded_height,
                        offsets,
                        byte_counts,
                    )?;
                    Ok((job, tile))
                })
                .collect::<Result<_, WsiError>>()?
        };

        for (job, tile) in decoded_tiles {
            let tile_data = tile.data.as_u8().ok_or_else(|| {
                WsiError::DisplayConversion(
                    "stitched Leica level requires interleaved U8 RGB tiles".into(),
                )
            })?;

            let src_x = (job.inter_left - job.tile_origin_x) as usize;
            let src_y = (job.inter_top - job.tile_origin_y) as usize;
            let dst_x = (job.inter_left - x) as usize;
            let dst_y = (job.inter_top - y) as usize;
            let copy_width = (job.inter_right - job.inter_left) as usize;
            let copy_height = (job.inter_bottom - job.inter_top) as usize;
            let src_stride = job.decoded_width as usize * 3;

            for copy_row in 0..copy_height {
                let src_off = (src_y + copy_row) * src_stride + src_x * 3;
                let dst_off = (dst_y + copy_row) * out_stride + dst_x * 3;
                let len = copy_width * 3;
                out[dst_off..dst_off + len].copy_from_slice(&tile_data[src_off..src_off + len]);
            }
        }

        Ok(CpuTile {
            width,
            height,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(out),
        })
    }
}
