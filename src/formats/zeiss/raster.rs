use super::*;

pub(super) fn rgb_u8_tile(width: u32, height: u32, data: Vec<u8>) -> Result<CpuTile, WsiError> {
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
                .expect("Bgr48 samples always have an even byte width");
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
