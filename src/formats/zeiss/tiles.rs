pub(super) use super::raster::bitmap_to_sample_buffer;
#[cfg(test)]
pub(super) use super::raster::blit_tile;
use super::raster::rgb_u8_tile;
#[cfg(test)]
pub(super) use super::raster::{blit_raw_uncompressed_rgb_subblock, blit_rgb_sample, RgbSample};
use super::slide::ZeissSlide;
#[cfg(test)]
use super::slide::ZEISS_LOCAL_TILE_HITS;

#[cfg(test)]
pub(super) use super::subblock::bitmap_from_raw_uncompressed_subblock;
use super::*;

impl ZeissSlide {
    pub(super) fn exact_raw_jpeg_subblock(
        &self,
        req: &TileRequest,
    ) -> Result<Option<czi_rs::DirectorySubBlockInfo>, WsiError> {
        if req.plane.get() != PlaneSelection::default() {
            return Ok(None);
        }
        let scene = self
            .dataset
            .scenes
            .get(req.scene.get())
            .ok_or(WsiError::SceneOutOfRange {
                index: req.scene.get(),
                count: self.dataset.scenes.len(),
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
                    count: series.levels.len() as u32,
                })?;
        let TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } = level.tile_layout
        else {
            return Ok(None);
        };
        if req.col < 0
            || req.row < 0
            || req.col >= tiles_across as i64
            || req.row >= tiles_down as i64
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

        let candidate_indices = self
            .canvas_level_tile_subblocks
            .get(req.level.get() as usize)
            .and_then(|tiles| tiles.get(&(req.col, req.row)));
        let Some([index]) = candidate_indices.map(Vec::as_slice) else {
            return Ok(None);
        };
        let info = {
            let czi = self.czi.lock().unwrap_or_else(|error| error.into_inner());
            czi.subblocks().get(*index).cloned().ok_or_else(|| {
                WsiError::DisplayConversion(format!("Zeiss subblock index {index} out of range"))
            })?
        };
        if info.compression != CziCompressionMode::Jpg {
            return Ok(None);
        }

        let downsample = level.downsample;
        if !downsample.is_finite()
            || downsample < 1.0
            || downsample > i64::MAX as f64
            || downsample.fract() != 0.0
        {
            return Ok(None);
        }
        let ratio = downsample as i64;
        let tile_x = req.col.checked_mul(i64::from(tile_width));
        let tile_y = req.row.checked_mul(i64::from(tile_height));
        let Some((tile_x, tile_y)) = tile_x.zip(tile_y) else {
            return Ok(None);
        };
        let tile_w = level
            .dimensions
            .0
            .saturating_sub(tile_x as u64)
            .min(u64::from(tile_width));
        let tile_h = level
            .dimensions
            .1
            .saturating_sub(tile_y as u64)
            .min(u64::from(tile_height));
        let expected_x = tile_x
            .checked_mul(ratio)
            .and_then(|x| i64::from(self.subblock_origin.0).checked_add(x));
        let expected_y = tile_y
            .checked_mul(ratio)
            .and_then(|y| i64::from(self.subblock_origin.1).checked_add(y));
        let expected_rect_w = i64::try_from(tile_w)
            .ok()
            .and_then(|width| width.checked_mul(ratio));
        let expected_rect_h = i64::try_from(tile_h)
            .ok()
            .and_then(|height| height.checked_mul(ratio));
        if expected_x != Some(i64::from(info.rect.x))
            || expected_y != Some(i64::from(info.rect.y))
            || expected_rect_w != Some(i64::from(info.rect.w))
            || expected_rect_h != Some(i64::from(info.rect.h))
            || u64::from(info.stored_size.w) != tile_w
            || u64::from(info.stored_size.h) != tile_h
        {
            return Ok(None);
        }

        Ok(Some(info))
    }

    pub(super) fn read_raw_jpeg_tile(
        &self,
        req: &TileRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        let Some(info) = self.exact_raw_jpeg_subblock(req)? else {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "Zeiss raw JPEG access requires one exact classic-JPEG subblock for tile ({}, {}) at level {}",
                    req.col,
                    req.row,
                    req.level.get()
                ),
            });
        };
        let raw = self.read_source_subblock(&info)?;
        let tile = crate::decode::jpeg::standalone_raw_jpeg_tile(raw.data)?;
        if (tile.width(), tile.height()) != (info.stored_size.w, info.stored_size.h) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "Zeiss raw JPEG dimensions {}x{} do not match stored subblock {}x{}",
                    tile.width(),
                    tile.height(),
                    info.stored_size.w,
                    info.stored_size.h
                ),
            });
        }
        Ok(tile)
    }

    pub(super) fn read_tile(
        &self,
        scene: usize,
        series: usize,
        level: u32,
        col: i64,
        row: i64,
        _backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let series_ref = self
            .dataset
            .scenes
            .get(scene)
            .and_then(|scene| scene.series.get(series))
            .ok_or(WsiError::SceneOutOfRange {
                index: scene,
                count: self.dataset.scenes.len(),
            })?;
        let level_ref = series_ref
            .levels
            .get(level as usize)
            .ok_or(WsiError::LevelOutOfRange {
                level,
                count: series_ref.levels.len() as u32,
            })?;
        let TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } = level_ref.tile_layout
        else {
            return Err(WsiError::UnsupportedFormat(
                "Zeiss levels must use regular tiles".into(),
            ));
        };
        if col < 0 || row < 0 || col >= tiles_across as i64 || row >= tiles_down as i64 {
            return Err(WsiError::TileRead {
                col,
                row,
                level,
                reason: format!(
                    "tile ({col},{row}) out of range ({}x{})",
                    tiles_across, tiles_down
                ),
            });
        }

        let key = (scene, level as usize, col, row);
        if let Some(cached) = self
            .tile_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            return Ok(cached.as_ref().clone());
        }

        let buffer =
            if let Some(buffer) = self.scene_tile_image_local(scene, level as usize, col, row)? {
                #[cfg(test)]
                ZEISS_LOCAL_TILE_HITS.fetch_add(1, Ordering::Relaxed);
                buffer
            } else {
                let level_img = self.scene_level_image(scene, level as usize)?;
                let x = (col as u32).saturating_mul(tile_width);
                let y = (row as u32).saturating_mul(tile_height);
                let w = tile_width.min(level_img.width.saturating_sub(x));
                let h = tile_height.min(level_img.height.saturating_sub(y));
                crop_rgb_interleaved_u8_buffer(level_img.as_ref(), x, y, w, h)?
            };
        let arc = Arc::new(buffer);
        let retained_bytes = u64::try_from(arc.data.byte_size()).unwrap_or(u64::MAX);
        self.tile_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(key, arc.clone(), retained_bytes);
        Ok(arc.as_ref().clone())
    }

    fn scene_tile_image_local(
        &self,
        scene: usize,
        level: usize,
        col: i64,
        row: i64,
    ) -> Result<Option<CpuTile>, WsiError> {
        let (_tile_width, _tile_height, tile_x, tile_y, tile_w, tile_h) = {
            let series = &self.dataset.scenes[scene].series[0];
            let level_ref = &series.levels[level];
            let TileLayout::Regular {
                tile_width,
                tile_height,
                ..
            } = level_ref.tile_layout
            else {
                return Ok(None);
            };
            let tile_x = (col as u64).saturating_mul(u64::from(tile_width));
            let tile_y = (row as u64).saturating_mul(u64::from(tile_height));
            // Each value is explicitly capped by its u32 tile dimension.
            let tile_w = level_ref
                .dimensions
                .0
                .saturating_sub(tile_x)
                .min(u64::from(tile_width)) as u32;
            let tile_h = level_ref
                .dimensions
                .1
                .saturating_sub(tile_y)
                .min(u64::from(tile_height)) as u32;
            (tile_width, tile_height, tile_x, tile_y, tile_w, tile_h)
        };
        let candidate_indices = self
            .canvas_level_tile_subblocks
            .get(level)
            .and_then(|tiles| tiles.get(&(col, row)).cloned())
            .unwrap_or_default();
        let tile_rgb_len = crate::core::limits::checked_product_to_usize(
            &[u64::from(tile_w), u64::from(tile_h), 3],
            crate::core::limits::MAX_DECODED_IMAGE_BYTES,
            "Zeiss RGB tile",
        )
        .map_err(WsiError::DisplayConversion)?;
        if candidate_indices.is_empty() {
            return rgb_u8_tile(tile_w, tile_h, vec![0; tile_rgb_len]).map(Some);
        }
        let _level_ratio = self.dataset.scenes[scene].series[0].levels[level]
            .downsample
            .round()
            .max(1.0) as i32;
        let tile_origin_x = i32::try_from(tile_x)
            .map_err(|_| WsiError::DisplayConversion("Zeiss tile x overflow".into()))?;
        let tile_origin_y = i32::try_from(tile_y)
            .map_err(|_| WsiError::DisplayConversion("Zeiss tile y overflow".into()))?;

        let candidate_infos = {
            let czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
            let all = czi.subblocks();
            let mut selected = Vec::new();
            for index in candidate_indices {
                let info = all.get(index).cloned().ok_or_else(|| {
                    WsiError::DisplayConversion(format!(
                        "Zeiss subblock index {index} out of range"
                    ))
                })?;
                if !matches!(
                    info.compression,
                    CziCompressionMode::UnCompressed
                        | CziCompressionMode::Jpg
                        | CziCompressionMode::JpgXr
                ) {
                    #[cfg(test)]
                    eprintln!(
                        "zeiss local tile: unsupported compression {:?} for subblock {index}",
                        info.compression
                    );
                    return Ok(None);
                }
                selected.push(info);
            }
            selected
        };
        let tile_rect = IntRect::new(
            tile_origin_x,
            tile_origin_y,
            i32::try_from(tile_w)
                .map_err(|_| WsiError::DisplayConversion("Zeiss tile width overflow".into()))?,
            i32::try_from(tile_h)
                .map_err(|_| WsiError::DisplayConversion("Zeiss tile height overflow".into()))?,
        );
        let subblocks: Vec<_> = candidate_infos
            .iter()
            .filter(|&info| {
                let global_rect = IntRect::new(
                    (info.rect.x - self.subblock_origin.0).div_euclid(_level_ratio),
                    (info.rect.y - self.subblock_origin.1).div_euclid(_level_ratio),
                    i32::try_from(info.stored_size.w).unwrap_or(i32::MAX),
                    i32::try_from(info.stored_size.h).unwrap_or(i32::MAX),
                );
                global_rect.intersect(tile_rect).is_some()
            })
            .cloned()
            .collect();
        if subblocks.is_empty() {
            #[cfg(test)]
            eprintln!(
                "zeiss local tile fallback: no subblocks intersect tile ({}, {}) level {}",
                tile_origin_x, tile_origin_y, level
            );
            // The candidate loop either returns early or selects every input,
            // and this branch is only reached from a nonempty candidate list.
            let pixel_type = candidate_infos[0].pixel_type;
            return czi_rs::Bitmap::zeros(pixel_type, tile_w, tile_h)
                .map_err(|source| WsiError::DisplayConversion(source.to_string()))
                .and_then(bitmap_to_sample_buffer)
                .map(Some);
        }

        let tile = self.compose_subblocks(
            &subblocks,
            (tile_w, tile_h),
            (tile_origin_x, tile_origin_y),
            _level_ratio,
        )?;
        if tile.data.as_u8().is_none() {
            return Err(WsiError::DisplayConversion(
                "Zeiss local tile path requires 8-bit RGB-compatible subblocks".into(),
            ));
        }
        Ok(Some(tile))
    }
}
