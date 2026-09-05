use super::*;

pub(super) fn validate_jpeg_transfer_syntax_frame(
    transfer_syntax_uid: &str,
    frame: &[u8],
) -> Result<(), WsiError> {
    let (sof, lossless_predictor) = jpeg_process_header(frame)?;
    let valid = match transfer_syntax_uid {
        uids::JPEG_BASELINE8_BIT => sof == j2k_jpeg::SofKind::Baseline8,
        uids::JPEG_EXTENDED12_BIT => {
            matches!(
                sof,
                j2k_jpeg::SofKind::Extended8 | j2k_jpeg::SofKind::Extended12
            )
        }
        JPEG_SPECTRAL_SELECTION_TRANSFER_SYNTAX | JPEG_FULL_PROGRESSION_TRANSFER_SYNTAX => {
            matches!(
                sof,
                j2k_jpeg::SofKind::Progressive8 | j2k_jpeg::SofKind::Progressive12
            )
        }
        uids::JPEG_LOSSLESS => sof == j2k_jpeg::SofKind::Lossless,
        uids::JPEG_LOSSLESS_SV1 => {
            sof == j2k_jpeg::SofKind::Lossless && lossless_predictor == Some(1)
        }
        _ => false,
    };
    if !valid {
        return Err(WsiError::Unsupported {
            reason: format!(
                "DICOM JPEG transfer syntax {transfer_syntax_uid} does not match encoded process {sof:?}"
            ),
        });
    }
    Ok(())
}

fn jpeg_process_header(frame: &[u8]) -> Result<(j2k_jpeg::SofKind, Option<u8>), WsiError> {
    let mut sof = None;
    for segment in j2k_jpeg::iter_segments(frame) {
        let segment = segment.map_err(|err| WsiError::Jpeg(err.to_string()))?;
        if j2k_jpeg::is_sof_marker(segment.marker) && sof.is_none() {
            sof = Some(
                j2k_jpeg::parse_sof_info(segment.marker, segment.payload)
                    .map_err(|err| WsiError::Jpeg(err.to_string()))?
                    .sof_kind,
            );
        }
        if segment.marker != 0xda {
            continue;
        }
        let component_count = segment.payload.first().copied().unwrap_or(0) as usize;
        let predictor_offset = 1usize
            .checked_add(component_count.checked_mul(2).ok_or_else(|| {
                WsiError::Jpeg("DICOM lossless JPEG SOS component count overflow".into())
            })?)
            .ok_or_else(|| WsiError::Jpeg("DICOM lossless JPEG SOS offset overflow".into()))?;
        let predictor = segment
            .payload
            .get(predictor_offset)
            .copied()
            .ok_or_else(|| WsiError::Jpeg("DICOM JPEG SOS is truncated".into()));
        return Ok((
            sof.ok_or_else(|| WsiError::Jpeg("DICOM JPEG frame has no SOF marker".into()))?,
            Some(predictor?),
        ));
    }
    Err(WsiError::Jpeg("DICOM JPEG frame has no SOS marker".into()))
}

pub(super) fn dicom_jpeg_color_transform(
    photometric_interpretation: &str,
) -> j2k_jpeg::ColorTransform {
    match photometric_interpretation {
        "RGB" => j2k_jpeg::ColorTransform::ForceRgb,
        "YBR_FULL" | "YBR_FULL_422" => j2k_jpeg::ColorTransform::ForceYCbCr,
        _ => j2k_jpeg::ColorTransform::Auto,
    }
}

pub(super) fn frame_bytes_to_rgb_tile(
    frame_bytes: &[u8],
    width: u32,
    height: u32,
    samples_per_pixel: u16,
    planar_configuration: u16,
    photometric_interpretation: &str,
) -> Result<CpuTile, WsiError> {
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| WsiError::DisplayConversion("DICOM frame dimensions overflow".into()))?;
    let rgb = match (samples_per_pixel, photometric_interpretation) {
        (3, "RGB") if planar_configuration == 0 => {
            let expected = pixel_count.checked_mul(3).ok_or_else(|| {
                WsiError::DisplayConversion("DICOM RGB frame size overflow".into())
            })?;
            if frame_bytes.len() != expected {
                return Err(WsiError::DisplayConversion(format!(
                    "DICOM RGB frame has {} bytes, expected {expected}",
                    frame_bytes.len()
                )));
            }
            frame_bytes.to_vec()
        }
        (3, "RGB") if planar_configuration == 1 => {
            let expected = pixel_count.checked_mul(3).ok_or_else(|| {
                WsiError::DisplayConversion("DICOM planar RGB frame size overflow".into())
            })?;
            if frame_bytes.len() != expected {
                return Err(WsiError::DisplayConversion(format!(
                    "DICOM planar RGB frame has {} bytes, expected {expected}",
                    frame_bytes.len()
                )));
            }
            let (r_plane, rest) = frame_bytes.split_at(pixel_count);
            let (g_plane, b_plane) = rest.split_at(pixel_count);
            let mut rgb = vec![0; expected];
            for idx in 0..pixel_count {
                let offset = idx * 3;
                rgb[offset] = r_plane[idx];
                rgb[offset + 1] = g_plane[idx];
                rgb[offset + 2] = b_plane[idx];
            }
            rgb
        }
        (1, "MONOCHROME1" | "MONOCHROME2") => {
            if frame_bytes.len() != pixel_count {
                return Err(WsiError::DisplayConversion(format!(
                    "DICOM monochrome frame has {} bytes, expected {pixel_count}",
                    frame_bytes.len()
                )));
            }
            let mut rgb = Vec::with_capacity(pixel_count * 3);
            for &gray in frame_bytes {
                // Preserve the legacy sv-slide behavior for consolidation:
                // MONOCHROME1 and MONOCHROME2 are both expanded without inversion.
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
            rgb
        }
        _ => {
            return Err(WsiError::DisplayConversion(format!(
                "unsupported DICOM pixel format: samples_per_pixel={samples_per_pixel}, photometric={photometric_interpretation}, planar_configuration={planar_configuration}"
            )));
        }
    };

    CpuTile::new(
        width,
        height,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(rgb),
    )
}

pub(super) fn decode_rle_lossless_frame(
    frame_bytes: &[u8],
    width: u32,
    height: u32,
    samples_per_pixel: u16,
    photometric_interpretation: &str,
) -> Result<CpuTile, WsiError> {
    if frame_bytes.len() < 64 {
        return Err(WsiError::DisplayConversion(
            "DICOM RLE frame is shorter than its 64-byte header".into(),
        ));
    }
    let segment_count = u32::from_le_bytes(frame_bytes[0..4].try_into().unwrap()) as usize;
    if segment_count == 0 || segment_count > 15 {
        return Err(WsiError::DisplayConversion(format!(
            "DICOM RLE segment count {segment_count} is invalid"
        )));
    }
    let expected_segments = match (samples_per_pixel, photometric_interpretation) {
        (3, "RGB") => 3usize,
        (1, "MONOCHROME1" | "MONOCHROME2") => 1usize,
        _ => {
            return Err(WsiError::DisplayConversion(format!(
                "unsupported DICOM RLE pixel format: samples_per_pixel={samples_per_pixel}, photometric={photometric_interpretation}"
            )));
        }
    };
    if segment_count < expected_segments {
        return Err(WsiError::DisplayConversion(format!(
            "DICOM RLE has {segment_count} segments, expected at least {expected_segments}"
        )));
    }
    let pixel_count_u64 = u64::from(width) * u64::from(height);
    let working_set_bytes = pixel_count_u64.saturating_mul(u64::from(samples_per_pixel) + 3);
    if working_set_bytes > crate::core::limits::MAX_DECODED_IMAGE_BYTES {
        return Err(WsiError::ResourceLimit {
            resource: "DICOM RLE decoded working set",
            requested: working_set_bytes,
            limit: crate::core::limits::MAX_DECODED_IMAGE_BYTES,
        });
    }
    let pixel_count = usize::try_from(pixel_count_u64).map_err(|_| WsiError::ResourceLimit {
        resource: "DICOM RLE pixel count",
        requested: pixel_count_u64,
        limit: usize::MAX as u64,
    })?;
    let mut planes = Vec::with_capacity(expected_segments);
    for segment in 0..expected_segments {
        let offset_start = 4 + segment * 4;
        let segment_start = u32::from_le_bytes(
            frame_bytes[offset_start..offset_start + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let segment_end = if segment + 1 < segment_count {
            let next_offset_start = 4 + (segment + 1) * 4;
            u32::from_le_bytes(
                frame_bytes[next_offset_start..next_offset_start + 4]
                    .try_into()
                    .unwrap(),
            ) as usize
        } else {
            frame_bytes.len()
        };
        if segment_start < 64 || segment_start > segment_end || segment_end > frame_bytes.len() {
            return Err(WsiError::DisplayConversion(format!(
                "DICOM RLE segment {segment} has invalid byte range {segment_start}..{segment_end}"
            )));
        }
        planes.push(decode_rle_segment(
            &frame_bytes[segment_start..segment_end],
            pixel_count,
        )?);
    }

    let rgb = match (samples_per_pixel, photometric_interpretation) {
        (3, "RGB") => {
            let rgb_len = pixel_count
                .checked_mul(3)
                .ok_or_else(|| WsiError::DisplayConversion("DICOM RLE RGB size overflow".into()))?;
            let mut rgb = try_zeroed_rle_buffer(rgb_len)?;
            for (idx, ((&red, &green), &blue)) in
                planes[0].iter().zip(&planes[1]).zip(&planes[2]).enumerate()
            {
                let offset = idx * 3;
                rgb[offset] = red;
                rgb[offset + 1] = green;
                rgb[offset + 2] = blue;
            }
            rgb
        }
        (1, "MONOCHROME1" | "MONOCHROME2") => {
            let rgb_len = pixel_count
                .checked_mul(3)
                .ok_or_else(|| WsiError::DisplayConversion("DICOM RLE RGB size overflow".into()))?;
            let mut rgb = try_rle_buffer_with_capacity(rgb_len)?;
            for &gray in &planes[0] {
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
            rgb
        }
        _ => unreachable!("DICOM RLE pixel format was validated before allocation"),
    };

    CpuTile::new(
        width,
        height,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(rgb),
    )
}

pub(super) fn decode_rle_segment(segment: &[u8], expected_len: usize) -> Result<Vec<u8>, WsiError> {
    let mut output = try_rle_buffer_with_capacity(expected_len)?;
    let mut i = 0;
    while i < segment.len() && output.len() < expected_len {
        let n = segment[i] as i8;
        i += 1;
        match n {
            0..=127 => {
                let count = n as usize + 1;
                let end = i.checked_add(count).ok_or_else(|| {
                    WsiError::DisplayConversion("DICOM RLE literal run overflow".into())
                })?;
                if end > segment.len() {
                    return Err(WsiError::DisplayConversion(
                        "DICOM RLE literal run exceeds segment length".into(),
                    ));
                }
                output.extend_from_slice(&segment[i..end]);
                i = end;
            }
            -127..=-1 => {
                if i >= segment.len() {
                    return Err(WsiError::DisplayConversion(
                        "DICOM RLE repeat run missing value".into(),
                    ));
                }
                let count = 1usize + (-n as usize);
                output.extend(std::iter::repeat_n(segment[i], count));
                i += 1;
            }
            -128 => {}
        }
    }
    if output.len() != expected_len {
        return Err(WsiError::DisplayConversion(format!(
            "DICOM RLE segment decoded to {} bytes, expected {expected_len}",
            output.len()
        )));
    }
    Ok(output)
}

fn try_rle_buffer_with_capacity(capacity: usize) -> Result<Vec<u8>, WsiError> {
    let mut output = Vec::new();
    output.try_reserve_exact(capacity).map_err(|_| {
        WsiError::DisplayConversion(format!(
            "unable to allocate {capacity} bytes for DICOM RLE decode"
        ))
    })?;
    Ok(output)
}

fn try_zeroed_rle_buffer(len: usize) -> Result<Vec<u8>, WsiError> {
    let mut output = try_rle_buffer_with_capacity(len)?;
    output.resize(len, 0);
    Ok(output)
}

pub(super) fn checked_dicom_tile_coordinates(
    col: i64,
    row: i64,
    level: u32,
    tiles_across: u32,
    tiles_down: u32,
) -> Result<(u32, u32), WsiError> {
    if col < 0 || row < 0 || col >= tiles_across as i64 || row >= tiles_down as i64 {
        return Err(WsiError::TileRead {
            col,
            row,
            level,
            reason: format!("tile ({col},{row}) out of range ({tiles_across}x{tiles_down})"),
        });
    }

    Ok((col as u32, row as u32))
}

pub(super) fn dicom_actual_tile_dimensions(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    col: u32,
    row: u32,
) -> (u32, u32) {
    let tile_x = col * tile_width;
    let tile_y = row * tile_height;
    let width = width.saturating_sub(tile_x).min(tile_width);
    let height = height.saturating_sub(tile_y).min(tile_height);
    (width, height)
}

pub(super) fn crop_sample_buffer_rgb(
    buffer: &CpuTile,
    width: u32,
    height: u32,
) -> Result<CpuTile, WsiError> {
    if buffer.width == width && buffer.height == height {
        return Ok(buffer.clone());
    }
    crop_rgb_interleaved_u8_buffer(buffer, 0, 0, width, height)
}

pub(super) fn crop_or_keep_sample_buffer_rgb(
    buffer: CpuTile,
    width: u32,
    height: u32,
) -> Result<CpuTile, WsiError> {
    if buffer.width == width && buffer.height == height {
        return Ok(buffer);
    }
    crop_sample_buffer_rgb(&buffer, width, height)
}

pub(super) fn raw_compression_for_transfer_syntax(
    transfer_syntax_uid: &str,
    photometric_interpretation: &str,
) -> Result<Compression, WsiError> {
    if is_jpeg_transfer_syntax(transfer_syntax_uid) {
        return Ok(Compression::Jpeg);
    }
    if JP2K_TRANSFER_SYNTAXES.contains(&transfer_syntax_uid) {
        return Ok(if jp2k_photometric_is_ycbcr(photometric_interpretation) {
            Compression::Jp2kYcbcr
        } else {
            Compression::Jp2kRgb
        });
    }
    Err(WsiError::Unsupported {
        reason: format!(
            "raw compressed DICOM tile access requires JPEG or J2K/HTJ2K transfer syntax, got {transfer_syntax_uid}"
        ),
    })
}

pub(super) fn jp2k_photometric_is_ycbcr(photometric_interpretation: &str) -> bool {
    matches!(
        photometric_interpretation,
        "YBR_FULL" | "YBR_FULL_422" | "YBR_ICT" | "YBR_RCT"
    )
}

pub(super) fn raw_photometric_interpretation(
    samples_per_pixel: u16,
    photometric_interpretation: &str,
) -> Result<EncodedTilePhotometricInterpretation, WsiError> {
    match (samples_per_pixel, photometric_interpretation) {
        (1, "MONOCHROME1" | "MONOCHROME2") => {
            Ok(EncodedTilePhotometricInterpretation::Monochrome2)
        }
        (3, "RGB") => Ok(EncodedTilePhotometricInterpretation::Rgb),
        (3, "YBR_FULL_422" | "YBR_FULL" | "YBR_ICT" | "YBR_RCT") => {
            Ok(EncodedTilePhotometricInterpretation::YbrFull422)
        }
        (_, other) => Err(WsiError::Unsupported {
            reason: format!(
                "raw compressed DICOM tile access does not support photometric interpretation {other}"
            ),
        }),
    }
}

pub(super) fn trim_encapsulated_frame_padding(data: &mut Vec<u8>) {
    if data.len() >= 3
        && data.last() == Some(&0)
        && data[data.len() - 3..data.len() - 1] == [0xFF, 0xD9]
    {
        data.pop();
    }
}

pub(super) fn black_sample_buffer(width: u32, height: u32) -> Result<CpuTile, WsiError> {
    let len = crate::core::limits::checked_product_to_usize(
        &[u64::from(width), u64::from(height), 3],
        crate::core::limits::MAX_DECODED_IMAGE_BYTES,
        "DICOM black tile",
    )
    .map_err(WsiError::DisplayConversion)?;
    CpuTile::new(
        width,
        height,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(vec![0; len]),
    )
}
