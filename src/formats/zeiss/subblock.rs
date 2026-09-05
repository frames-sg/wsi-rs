use super::*;

pub(super) fn bitmap_from_raw_uncompressed_subblock(
    raw: &czi_rs::RawSubBlock,
) -> Result<czi_rs::Bitmap, WsiError> {
    if raw.info.compression != CziCompressionMode::UnCompressed {
        return Err(WsiError::DisplayConversion(format!(
            "unsupported Zeiss compression {}",
            raw.info.compression.as_str()
        )));
    }
    let Some(stride) =
        (raw.info.stored_size.w as usize).checked_mul(raw.info.pixel_type.bytes_per_pixel())
    else {
        return Err(WsiError::DisplayConversion(
            "Zeiss bitmap size overflow".into(),
        ));
    };
    let Some(expected_len) = stride.checked_mul(raw.info.stored_size.h as usize) else {
        return Err(WsiError::DisplayConversion(
            "Zeiss bitmap size overflow".into(),
        ));
    };
    if raw.data.len() != expected_len {
        return Err(WsiError::DisplayConversion(format!(
            "Zeiss uncompressed payload length {} does not match expected {expected_len}",
            raw.data.len()
        )));
    }
    Ok(czi_rs::Bitmap {
        pixel_type: raw.info.pixel_type,
        width: raw.info.stored_size.w,
        height: raw.info.stored_size.h,
        stride,
        data: raw.data.clone(),
    })
}

pub(super) fn bitmap_from_raw_subblock(
    raw: &czi_rs::RawSubBlock,
    limits: crate::SlideLimits,
) -> Result<czi_rs::Bitmap, WsiError> {
    if raw.info.compression == CziCompressionMode::UnCompressed {
        return bitmap_from_raw_uncompressed_subblock(raw);
    }
    // Embedded CZI attachment composition still uses the container's BGR bitmap.
    // WSI composition consumes the codec's RGB tile directly.
    let tile = tile_from_raw_subblock(raw, limits)?;
    let bgr = match &tile.data {
        CpuTileData::U8(values) => values
            .chunks_exact(3)
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect(),
        CpuTileData::U16(values) => values
            .chunks_exact(3)
            .flat_map(|p| [p[2], p[1], p[0]])
            .flat_map(u16::to_le_bytes)
            .collect(),
        _ => {
            return Err(WsiError::DisplayConversion(
                "CZI subblock did not return integer RGB samples".into(),
            ))
        }
    };
    czi_rs::Bitmap::new(raw.info.pixel_type, tile.width, tile.height, bgr)
        .map_err(|e| WsiError::DisplayConversion(e.to_string()))
}

pub(super) fn tile_from_raw_subblock(
    raw: &czi_rs::RawSubBlock,
    limits: crate::SlideLimits,
) -> Result<CpuTile, WsiError> {
    match raw.info.compression {
        CziCompressionMode::UnCompressed => {
            super::raster::bitmap_to_sample_buffer(bitmap_from_raw_uncompressed_subblock(raw)?)
        }
        CziCompressionMode::Jpg => tile_from_raw_jpeg_subblock(raw),
        CziCompressionMode::JpgXr => tile_from_raw_jpegxr_subblock(raw, limits),
        other => Err(WsiError::DisplayConversion(format!(
            "unsupported Zeiss compression {}",
            other.as_str()
        ))),
    }
}

fn tile_from_raw_jpeg_subblock(raw: &czi_rs::RawSubBlock) -> Result<CpuTile, WsiError> {
    if raw.info.pixel_type != CziPixelType::Bgr24 {
        return Err(WsiError::DisplayConversion(format!(
            "Zeiss JPEG subblocks require Bgr24 pixels, got {:?}",
            raw.info.pixel_type
        )));
    }

    let expected_width = raw.info.stored_size.w;
    let expected_height = raw.info.stored_size.h;
    if expected_width == 0 || expected_height == 0 {
        return Err(WsiError::DisplayConversion(
            "Zeiss JPEG subblock has zero stored dimensions".into(),
        ));
    }

    let decoded = crate::core::batch::exactly_one(
        decode_batch_jpeg(&[JpegDecodeJob {
            data: Cow::Borrowed(&raw.data),
            tables: None,
            expected_width,
            expected_height,
            color_transform: j2k_jpeg::ColorTransform::Auto,
            force_dimensions: false,
            requested_size: None,
        }]),
        "Zeiss JPEG subblock decode",
    )?
    .map_err(|error| {
        WsiError::DisplayConversion(format!(
            "failed to decode Zeiss JPEG subblock at file offset {}: {error}",
            raw.info.file_position
        ))
    })?;

    if (decoded.width, decoded.height) != (expected_width, expected_height) {
        return Err(WsiError::DisplayConversion(format!(
            "Zeiss JPEG subblock decoded as {}x{} but CZI stored geometry is {}x{}",
            decoded.width, decoded.height, expected_width, expected_height
        )));
    }
    decoded.data.as_u8().ok_or_else(|| {
        WsiError::DisplayConversion("Zeiss JPEG subblock did not decode to 8-bit RGB".into())
    })?;
    Ok(decoded)
}

fn tile_from_raw_jpegxr_subblock(
    raw: &czi_rs::RawSubBlock,
    limits: crate::SlideLimits,
) -> Result<CpuTile, WsiError> {
    let sample_type = match raw.info.pixel_type {
        CziPixelType::Bgr24 => SampleType::Uint8,
        CziPixelType::Bgr48 => SampleType::Uint16,
        _ => {
            return Err(WsiError::UnsupportedFormat(
                "CZI JPEG XR currently requires Bgr24 or Bgr48 pixels".into(),
            ))
        }
    };
    crate::decode::jpegxr::decode_jpegxr(
        &raw.data,
        raw.info.stored_size.w,
        raw.info.stored_size.h,
        sample_type,
        3,
        limits,
    )
}
