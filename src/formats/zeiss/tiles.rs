use super::slide::ZeissSlide;
#[cfg(test)]
use super::slide::{
    ZEISS_DIRECT_LEVEL_COMPOSE_HITS, ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS, ZEISS_LOCAL_TILE_HITS,
};
use super::*;

impl ZeissSlide {
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
        self.tile_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(key, arc.clone());
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
            let tile_w = u32::try_from(
                level_ref
                    .dimensions
                    .0
                    .saturating_sub(tile_x)
                    .min(u64::from(tile_width)),
            )
            .map_err(|_| WsiError::DisplayConversion("Zeiss tile width overflow".into()))?;
            let tile_h = u32::try_from(
                level_ref
                    .dimensions
                    .1
                    .saturating_sub(tile_y)
                    .min(u64::from(tile_height)),
            )
            .map_err(|_| WsiError::DisplayConversion("Zeiss tile height overflow".into()))?;
            (tile_width, tile_height, tile_x, tile_y, tile_w, tile_h)
        };
        let candidate_indices = self
            .canvas_level_tile_subblocks
            .get(level)
            .and_then(|tiles| tiles.get(&(col, row)).cloned())
            .unwrap_or_default();
        if candidate_indices.is_empty() {
            return rgb_u8_tile(
                tile_w,
                tile_h,
                vec![0; tile_w as usize * tile_h as usize * 3],
            )
            .map(Some);
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
                if info.compression != CziCompressionMode::UnCompressed {
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
            let pixel_type = candidate_infos
                .first()
                .map(|info| info.pixel_type)
                .ok_or_else(|| {
                    WsiError::DisplayConversion(
                        "Zeiss local tile path lost candidate pixel type".into(),
                    )
                })?;
            return czi_rs::Bitmap::zeros(pixel_type, tile_w, tile_h)
                .map_err(|source| WsiError::DisplayConversion(source.to_string()))
                .and_then(bitmap_to_sample_buffer)
                .map(Some);
        }

        let direct_uncompressed_rgb = subblocks
            .iter()
            .all(|info| matches!(info.pixel_type, CziPixelType::Bgr24 | CziPixelType::Bgra32));
        let mut czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
        if direct_uncompressed_rgb {
            let mut destination = vec![0u8; tile_w as usize * tile_h as usize * 3];
            for info in subblocks {
                let raw = czi
                    .read_subblock(info.index)
                    .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
                blit_raw_uncompressed_rgb_subblock(
                    &mut destination,
                    tile_w,
                    tile_h,
                    &raw,
                    (info.rect.x - self.subblock_origin.0).div_euclid(_level_ratio) - tile_origin_x,
                    (info.rect.y - self.subblock_origin.1).div_euclid(_level_ratio) - tile_origin_y,
                )?;
            }
            #[cfg(test)]
            ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS.fetch_add(1, Ordering::Relaxed);
            return rgb_u8_tile(tile_w, tile_h, destination).map(Some);
        }

        let mut destination = vec![0u8; tile_w as usize * tile_h as usize * 3];
        for info in subblocks {
            let raw = czi
                .read_subblock(info.index)
                .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
            let bitmap = bitmap_from_raw_uncompressed_subblock(&raw)?;
            let sample = bitmap_to_sample_buffer(bitmap)?;
            let sample_data = sample.data.as_u8().ok_or_else(|| {
                WsiError::DisplayConversion(
                    "Zeiss local tile path requires 8-bit RGB-compatible subblocks".into(),
                )
            })?;
            let blit_x =
                (info.rect.x - self.subblock_origin.0).div_euclid(_level_ratio) - tile_origin_x;
            let blit_y =
                (info.rect.y - self.subblock_origin.1).div_euclid(_level_ratio) - tile_origin_y;
            blit_rgb_sample(
                &mut destination,
                (tile_w, tile_h),
                RgbSample {
                    width: sample.width,
                    height: sample.height,
                    data: sample_data,
                },
                (blit_x, blit_y),
            )?;
        }

        rgb_u8_tile(tile_w, tile_h, destination).map(Some)
    }

    pub(super) fn scene_level_image(
        &self,
        scene: usize,
        level: usize,
    ) -> Result<Arc<CpuTile>, WsiError> {
        if let Some(cached) = self
            .level_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(scene, level))
            .cloned()
        {
            return Ok(cached);
        }

        let series = &self.dataset.scenes[scene].series[0];
        let level_ref = &series.levels[level];
        let buffer = if let Some(buffer) = self.scene_level_image_from_subblocks(scene, level)? {
            #[cfg(test)]
            ZEISS_DIRECT_LEVEL_COMPOSE_HITS.fetch_add(1, Ordering::Relaxed);
            buffer
        } else if level == 0 {
            return Err(WsiError::UnsupportedFormat(
                "Zeiss level 0 requires direct subblock composition".into(),
            ));
        } else {
            let base = self.scene_level_image(scene, 0)?;
            let rgb = base.as_ref().clone().into_rgb()?;
            let resized = imageops::resize(
                &rgb,
                level_ref.dimensions.0 as u32,
                level_ref.dimensions.1 as u32,
                FilterType::Triangle,
            );
            rgb_u8_tile(resized.width(), resized.height(), resized.into_raw())?
        };
        let arc = Arc::new(buffer);
        self.level_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put((scene, level), arc.clone());
        Ok(arc)
    }

    fn scene_level_image_from_subblocks(
        &self,
        scene: usize,
        level: usize,
    ) -> Result<Option<CpuTile>, WsiError> {
        let candidate_indices = self
            .canvas_level_subblocks
            .get(level)
            .cloned()
            .unwrap_or_default();
        if candidate_indices.is_empty() {
            return Ok(None);
        }

        let candidate_infos = {
            let czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
            let all = czi.subblocks();
            let mut selected = Vec::with_capacity(candidate_indices.len());
            for index in candidate_indices {
                let info = all.get(index).cloned().ok_or_else(|| {
                    WsiError::DisplayConversion(format!(
                        "Zeiss subblock index {index} out of range"
                    ))
                })?;
                if info.compression != CziCompressionMode::UnCompressed {
                    return Ok(None);
                }
                selected.push(info);
            }
            selected
        };

        if candidate_infos.is_empty() {
            return Ok(None);
        }

        let series = &self.dataset.scenes[scene].series[0];
        let level_ref = &series.levels[level];

        let mut subblocks = candidate_infos;
        subblocks.sort_by_key(|info| (info.m_index.unwrap_or(i32::MIN), info.file_position));

        let direct_uncompressed_rgb = subblocks
            .iter()
            .all(|info| matches!(info.pixel_type, CziPixelType::Bgr24 | CziPixelType::Bgra32));
        let level_ratio = level_ref.downsample.round().max(1.0) as i32;
        if direct_uncompressed_rgb {
            let mut czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
            let mut destination =
                vec![0u8; level_ref.dimensions.0 as usize * level_ref.dimensions.1 as usize * 3];
            for info in subblocks {
                let raw = czi
                    .read_subblock(info.index)
                    .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
                blit_raw_uncompressed_rgb_subblock(
                    &mut destination,
                    level_ref.dimensions.0 as u32,
                    level_ref.dimensions.1 as u32,
                    &raw,
                    (info.rect.x - self.subblock_origin.0).div_euclid(level_ratio),
                    (info.rect.y - self.subblock_origin.1).div_euclid(level_ratio),
                )?;
            }
            #[cfg(test)]
            ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS.fetch_add(1, Ordering::Relaxed);
            return rgb_u8_tile(
                level_ref.dimensions.0 as u32,
                level_ref.dimensions.1 as u32,
                destination,
            )
            .map(Some);
        }

        let mut destination: Option<czi_rs::Bitmap> = None;
        for info in subblocks {
            let raw = {
                let mut czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
                czi.read_subblock(info.index)
                    .map_err(|source| WsiError::DisplayConversion(source.to_string()))?
            };
            let bitmap = bitmap_from_raw_uncompressed_subblock(&raw)?;
            let blit_x = (info.rect.x - self.subblock_origin.0).div_euclid(level_ratio);
            let blit_y = (info.rect.y - self.subblock_origin.1).div_euclid(level_ratio);
            match destination.as_mut() {
                Some(destination_bitmap) => {
                    if destination_bitmap.pixel_type != bitmap.pixel_type {
                        return Ok(None);
                    }
                    blit_tile(destination_bitmap, &bitmap, blit_x, blit_y)?;
                }
                None => {
                    let mut destination_bitmap = czi_rs::Bitmap::zeros(
                        bitmap.pixel_type,
                        level_ref.dimensions.0 as u32,
                        level_ref.dimensions.1 as u32,
                    )
                    .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
                    blit_tile(&mut destination_bitmap, &bitmap, blit_x, blit_y)?;
                    destination = Some(destination_bitmap);
                }
            }
        }

        destination.map(bitmap_to_sample_buffer).transpose()
    }
}

fn rgb_u8_tile(width: u32, height: u32, data: Vec<u8>) -> Result<CpuTile, WsiError> {
    CpuTile::new(
        width,
        height,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(data),
    )
}

pub(super) fn blit_tile(
    destination: &mut czi_rs::Bitmap,
    source: &czi_rs::Bitmap,
    offset_x: i32,
    offset_y: i32,
) -> Result<(), WsiError> {
    if destination.pixel_type != source.pixel_type {
        return Err(WsiError::DisplayConversion(
            "cannot compose Zeiss tiles with mismatched pixel types".into(),
        ));
    }

    let source_rect = IntRect::new(
        offset_x,
        offset_y,
        source.width as i32,
        source.height as i32,
    );
    let destination_rect = IntRect::new(0, 0, destination.width as i32, destination.height as i32);
    let Some(intersection) = source_rect.intersect(destination_rect) else {
        return Ok(());
    };

    let bytes_per_pixel = destination.pixel_type.bytes_per_pixel();
    for row in 0..intersection.h as usize {
        let src_x = (intersection.x - offset_x) as usize;
        let src_y = (intersection.y - offset_y) as usize + row;
        let dst_x = intersection.x as usize;
        let dst_y = intersection.y as usize + row;
        let row_bytes = intersection.w as usize * bytes_per_pixel;

        let src_offset = src_y
            .checked_mul(source.stride)
            .and_then(|value| value.checked_add(src_x * bytes_per_pixel))
            .ok_or_else(|| {
                WsiError::DisplayConversion("Zeiss source tile offset overflow".into())
            })?;
        let dst_offset = dst_y
            .checked_mul(destination.stride)
            .and_then(|value| value.checked_add(dst_x * bytes_per_pixel))
            .ok_or_else(|| {
                WsiError::DisplayConversion("Zeiss destination tile offset overflow".into())
            })?;

        destination.data[dst_offset..dst_offset + row_bytes]
            .copy_from_slice(&source.data[src_offset..src_offset + row_bytes]);
    }

    Ok(())
}

pub(super) struct RgbSample<'a> {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) data: &'a [u8],
}

pub(super) fn blit_rgb_sample(
    destination: &mut [u8],
    dest_size: (u32, u32),
    source: RgbSample<'_>,
    offset: (i32, i32),
) -> Result<(), WsiError> {
    let (dest_width, dest_height) = dest_size;
    let (offset_x, offset_y) = offset;
    let source_rect = IntRect::new(
        offset_x,
        offset_y,
        source.width as i32,
        source.height as i32,
    );
    let destination_rect = IntRect::new(0, 0, dest_width as i32, dest_height as i32);
    let Some(intersection) = source_rect.intersect(destination_rect) else {
        return Ok(());
    };

    let src_stride = source.width as usize * 3;
    let dest_stride = dest_width as usize * 3;
    for row in 0..intersection.h as usize {
        let src_x = (intersection.x - offset_x) as usize;
        let src_y = (intersection.y - offset_y) as usize + row;
        let dst_x = intersection.x as usize;
        let dst_y = intersection.y as usize + row;
        let row_bytes = intersection.w as usize * 3;

        let src_offset = src_y
            .checked_mul(src_stride)
            .and_then(|value| value.checked_add(src_x * 3))
            .ok_or_else(|| {
                WsiError::DisplayConversion("Zeiss source RGB tile offset overflow".into())
            })?;
        let dst_offset = dst_y
            .checked_mul(dest_stride)
            .and_then(|value| value.checked_add(dst_x * 3))
            .ok_or_else(|| {
                WsiError::DisplayConversion("Zeiss destination RGB tile offset overflow".into())
            })?;
        destination[dst_offset..dst_offset + row_bytes]
            .copy_from_slice(&source.data[src_offset..src_offset + row_bytes]);
    }

    Ok(())
}

pub(super) fn blit_raw_uncompressed_rgb_subblock(
    destination: &mut [u8],
    dest_width: u32,
    dest_height: u32,
    raw: &czi_rs::RawSubBlock,
    offset_x: i32,
    offset_y: i32,
) -> Result<(), WsiError> {
    let source_width = raw.info.stored_size.w;
    let source_height = raw.info.stored_size.h;
    let source_rect = IntRect::new(
        offset_x,
        offset_y,
        source_width as i32,
        source_height as i32,
    );
    let destination_rect = IntRect::new(0, 0, dest_width as i32, dest_height as i32);
    let Some(intersection) = source_rect.intersect(destination_rect) else {
        return Ok(());
    };

    let source_bytes = raw.data.as_slice();
    let source_stride = source_width as usize
        * match raw.info.pixel_type {
            CziPixelType::Bgr24 => 3,
            CziPixelType::Bgra32 => 4,
            other => {
                return Err(WsiError::DisplayConversion(format!(
                    "unsupported Zeiss direct blit pixel type {other:?}"
                )));
            }
        };
    let dest_stride = dest_width as usize * 3;
    let bytes_per_pixel = source_stride / source_width as usize;
    let source_needed = source_stride * source_height as usize;
    if source_bytes.len() < source_needed {
        return Err(WsiError::DisplayConversion(
            "Zeiss raw subblock shorter than expected".into(),
        ));
    }

    for row in 0..intersection.h as usize {
        let src_x = (intersection.x - offset_x) as usize;
        let src_y = (intersection.y - offset_y) as usize + row;
        let dst_x = intersection.x as usize;
        let dst_y = intersection.y as usize + row;
        let src_offset = src_y
            .checked_mul(source_stride)
            .and_then(|value| value.checked_add(src_x * bytes_per_pixel))
            .ok_or_else(|| {
                WsiError::DisplayConversion("Zeiss raw source offset overflow".into())
            })?;
        let dst_offset = dst_y
            .checked_mul(dest_stride)
            .and_then(|value| value.checked_add(dst_x * 3))
            .ok_or_else(|| {
                WsiError::DisplayConversion("Zeiss raw destination offset overflow".into())
            })?;
        match raw.info.pixel_type {
            CziPixelType::Bgr24 => {
                let src_row = &source_bytes[src_offset..src_offset + intersection.w as usize * 3];
                let dst_row =
                    &mut destination[dst_offset..dst_offset + intersection.w as usize * 3];
                for (src_px, dst_px) in src_row.chunks_exact(3).zip(dst_row.chunks_exact_mut(3)) {
                    dst_px[0] = src_px[2];
                    dst_px[1] = src_px[1];
                    dst_px[2] = src_px[0];
                }
            }
            CziPixelType::Bgra32 => {
                let src_row = &source_bytes[src_offset..src_offset + intersection.w as usize * 4];
                let dst_row =
                    &mut destination[dst_offset..dst_offset + intersection.w as usize * 3];
                for (src_px, dst_px) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(3)) {
                    dst_px[0] = src_px[2];
                    dst_px[1] = src_px[1];
                    dst_px[2] = src_px[0];
                }
            }
            other => {
                return Err(WsiError::DisplayConversion(format!(
                    "unsupported Zeiss direct blit pixel type {other:?}"
                )));
            }
        }
    }

    Ok(())
}

pub(super) fn bitmap_to_sample_buffer(bitmap: czi_rs::Bitmap) -> Result<CpuTile, WsiError> {
    match bitmap.pixel_type {
        CziPixelType::Bgr24 => {
            let mut rgb = Vec::with_capacity(bitmap.data.len());
            for chunk in bitmap.data.chunks_exact(3) {
                rgb.extend_from_slice(&[chunk[2], chunk[1], chunk[0]]);
            }
            rgb_u8_tile(bitmap.width, bitmap.height, rgb)
        }
        CziPixelType::Bgra32 => {
            let mut rgb =
                Vec::with_capacity((bitmap.width as usize) * (bitmap.height as usize) * 3);
            for chunk in bitmap.data.chunks_exact(4) {
                rgb.extend_from_slice(&[chunk[2], chunk[1], chunk[0]]);
            }
            rgb_u8_tile(bitmap.width, bitmap.height, rgb)
        }
        CziPixelType::Bgr48 => {
            let values = bitmap
                .to_u16_vec()
                .map_err(|err| WsiError::DisplayConversion(err.to_string()))?;
            let mut rgb = Vec::with_capacity(values.len());
            for chunk in values.chunks_exact(3) {
                rgb.extend_from_slice(&[chunk[2], chunk[1], chunk[0]]);
            }
            CpuTile::new(
                bitmap.width,
                bitmap.height,
                3,
                ColorSpace::Rgb,
                CpuTileLayout::Interleaved,
                CpuTileData::u16(rgb),
            )
        }
        other => Err(WsiError::DisplayConversion(format!(
            "unsupported Zeiss pixel type {other:?}"
        ))),
    }
}

pub(super) fn bitmap_from_raw_uncompressed_subblock(
    raw: &czi_rs::RawSubBlock,
) -> Result<czi_rs::Bitmap, WsiError> {
    if raw.info.compression != CziCompressionMode::UnCompressed {
        return Err(WsiError::DisplayConversion(format!(
            "unsupported Zeiss compression {}",
            raw.info.compression.as_str()
        )));
    }
    let expected_len = (raw.info.stored_size.w as usize)
        .checked_mul(raw.info.stored_size.h as usize)
        .and_then(|value| value.checked_mul(raw.info.pixel_type.bytes_per_pixel()))
        .ok_or_else(|| WsiError::DisplayConversion("Zeiss bitmap size overflow".into()))?;
    let mut decoded = raw.data.clone();
    if decoded.len() < expected_len {
        decoded.resize(expected_len, 0);
    } else {
        decoded.truncate(expected_len);
    }
    czi_rs::Bitmap::new(
        raw.info.pixel_type,
        raw.info.stored_size.w,
        raw.info.stored_size.h,
        decoded,
    )
    .map_err(|source| WsiError::DisplayConversion(source.to_string()))
}
