use super::transfer_syntax::DicomTransferSyntax;
use super::*;
use crate::core::types::ycbcr::ycbcr_to_rgb_u16;
use crate::decode::jpeg::{decode_batch_jpeg, decode_batch_jpeg_rgb16, JpegDecodeJob};
use j2k::CpuDecodeParallelism;

/// Checks that a frame's coding process matches its JPEG transfer syntax
/// and its precision matches BitsStored. Frames of other syntaxes never match.
pub(super) fn validate_jpeg_transfer_syntax_frame(
    transfer_syntax: DicomTransferSyntax,
    transfer_syntax_uid: &str,
    bit_depth: DicomBitDepth,
    frame: &[u8],
) -> Result<(), WsiError> {
    let (sof, precision, lossless_predictor) = jpeg_process_header(frame)?;
    let valid = match transfer_syntax {
        DicomTransferSyntax::Jpeg(process) => process.accepts(sof, lossless_predictor),
        DicomTransferSyntax::Native { .. }
        | DicomTransferSyntax::Jp2k { .. }
        | DicomTransferSyntax::Rle => false,
    };
    if !valid {
        return Err(WsiError::Unsupported {
            reason: format!(
                "DICOM JPEG transfer syntax {transfer_syntax_uid} does not match encoded process {sof:?}"
            ),
        });
    }
    if precision != bit_depth.bits_stored() {
        return Err(WsiError::Unsupported {
            reason: format!(
                "DICOM JPEG frame precision {precision} does not match BitsStored {}",
                bit_depth.bits_stored()
            ),
        });
    }
    Ok(())
}

/// Returns the frame's SOF process, sample precision and SOS predictor byte.
fn jpeg_process_header(frame: &[u8]) -> Result<(j2k_jpeg::SofKind, u8, Option<u8>), WsiError> {
    let mut sof = None;
    for segment in j2k_jpeg::iter_segments(frame) {
        let segment = segment.map_err(|err| WsiError::Jpeg(err.to_string()))?;
        if j2k_jpeg::is_sof_marker(segment.marker) && sof.is_none() {
            let info = j2k_jpeg::parse_sof_info(segment.marker, segment.payload)
                .map_err(|err| WsiError::Jpeg(err.to_string()))?;
            sof = Some((info.sof_kind, info.bit_depth));
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
        let (sof, precision) =
            sof.ok_or_else(|| WsiError::Jpeg("DICOM JPEG frame has no SOF marker".into()))?;
        return Ok((sof, precision, Some(predictor?)));
    }
    Err(WsiError::Jpeg("DICOM JPEG frame has no SOS marker".into()))
}

/// Decode JPEG frames at their validated DICOM precision: 8-bit frames keep the
/// U8 RGB path, frames stored in 16-bit words decode to U16 RGB.
pub(super) fn decode_dicom_jpeg_batch(
    bit_depth: DicomBitDepth,
    jobs: &[JpegDecodeJob<'_>],
) -> Vec<Result<CpuTile, WsiError>> {
    match bit_depth {
        DicomBitDepth::Eight => decode_batch_jpeg(jobs),
        DicomBitDepth::Uint16 { .. } => decode_batch_jpeg_rgb16(jobs),
    }
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

pub(super) fn decode_dicom_jp2k_u16(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    samples_per_pixel: u16,
    photometric_interpretation: &str,
    bits_stored: u8,
    parallelism: CpuDecodeParallelism,
) -> Result<CpuTile, WsiError> {
    let view = j2k::J2kView::parse(data).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    let header = crate::decode::jp2k_codestream::codestream_header_from_view(&view)?;
    if header.image_width < expected_width || header.image_height < expected_height {
        return Err(WsiError::Jp2k(format!(
            "dimension mismatch: expected at least {expected_width}x{expected_height}, got {}x{}",
            header.image_width, header.image_height
        )));
    }
    let component_count = match (samples_per_pixel, photometric_interpretation) {
        (1, "MONOCHROME1" | "MONOCHROME2") => 1usize,
        (3, "RGB" | "YBR_FULL" | "YBR_FULL_422" | "YBR_ICT" | "YBR_RCT") => 3usize,
        _ => {
            return Err(WsiError::Jp2k(format!(
                "unsupported DICOM JPEG 2000 16-bit pixel format: samples_per_pixel={samples_per_pixel}, photometric={photometric_interpretation}"
            )));
        }
    };
    if header.components.len() != component_count
        || header
            .components
            .iter()
            .any(|component| component.signed || component.bit_depth != bits_stored)
    {
        return Err(WsiError::Jp2k(format!(
            "DICOM JPEG 2000 output requires {component_count} unsigned {bits_stored}-bit component(s)"
        )));
    }
    let source_samples = checked_sample_len(
        header.image_width,
        header.image_height,
        component_count,
        2,
        "decoded DICOM JPEG 2000 source image",
    )
    .map_err(WsiError::Jp2k)?;
    let rgb_samples = checked_sample_len(
        header.image_width,
        header.image_height,
        3,
        2,
        "decoded DICOM JPEG 2000 image",
    )
    .map_err(WsiError::Jp2k)?;
    // j2k rejects empty images, so the checked source size bounds each row.
    let row_bytes = header.image_width as usize * component_count * 2;
    let mut bytes = vec![0; source_samples * 2];
    let mut decoder =
        j2k::J2kDecoder::from_view(view).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    decoder.set_cpu_decode_parallelism(parallelism);
    decoder
        .decode_into(
            &mut bytes,
            row_bytes,
            if component_count == 1 {
                j2k_core::PixelFormat::Gray16
            } else {
                j2k_core::PixelFormat::Rgb16
            },
        )
        .map_err(|error| WsiError::Jp2k(format!("j2k JP2K decode failed: {error}")))?;
    // j2k writes 16-bit samples little-endian.
    let words = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| u16::from_le_bytes(*sample));
    let mut samples = Vec::with_capacity(rgb_samples);
    if component_count == 1 {
        samples.extend(words.flat_map(|gray| [gray, gray, gray]));
    } else {
        samples.extend(words);
    }
    if component_count == 3
        && jp2k_photometric_is_ycbcr(photometric_interpretation)
        && !header.multiple_component_transform
    {
        convert_ybr_full_u16_to_rgb(&mut samples, bits_stored);
    }
    CpuTile::new(
        header.image_width,
        header.image_height,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u16(samples),
    )
}

fn convert_ybr_full_u16_to_rgb(samples: &mut [u16], bits_stored: u8) {
    for pixel in samples.as_chunks_mut::<3>().0 {
        *pixel = ycbcr_to_rgb_u16(pixel[0], pixel[1], pixel[2], bits_stored);
    }
}

/// Interleaved RGB samples of a native frame: RGB as stored, planar RGB
/// interleaved, or monochrome expanded to gray RGB.
pub(super) fn interleaved_rgb_samples<T: Copy>(
    samples: Vec<T>,
    width: u32,
    height: u32,
    samples_per_pixel: u16,
    planar_configuration: u16,
    photometric_interpretation: &str,
) -> Result<Vec<T>, WsiError> {
    let bits = size_of::<T>() * 8;
    let rgb_len = checked_sample_len(width, height, 3, size_of::<T>(), "decoded DICOM RGB frame")
        .map_err(WsiError::DisplayConversion)?;
    let pixel_count = rgb_len / 3;
    let expect_len = |what: &str, expected: usize| {
        if samples.len() == expected {
            Ok(())
        } else {
            Err(WsiError::DisplayConversion(format!(
                "DICOM {bits}-bit {what} frame has {} samples, expected {expected}",
                samples.len()
            )))
        }
    };
    match (samples_per_pixel, photometric_interpretation) {
        (3, "RGB") if planar_configuration == 0 => {
            expect_len("RGB", rgb_len)?;
            Ok(samples)
        }
        (3, "RGB") if planar_configuration == 1 => {
            expect_len("planar RGB", rgb_len)?;
            let (r_plane, rest) = samples.split_at(pixel_count);
            let (g_plane, b_plane) = rest.split_at(pixel_count);
            Ok(r_plane
                .iter()
                .zip(g_plane)
                .zip(b_plane)
                .flat_map(|((&r, &g), &b)| [r, g, b])
                .collect())
        }
        (1, "MONOCHROME1" | "MONOCHROME2") => {
            expect_len("monochrome", pixel_count)?;
            // MONOCHROME1 and MONOCHROME2 are both expanded without inversion,
            // preserving the legacy sv-slide behavior.
            Ok(samples.iter().flat_map(|&gray| [gray, gray, gray]).collect())
        }
        _ => Err(WsiError::DisplayConversion(format!(
            "unsupported DICOM {bits}-bit pixel format: samples_per_pixel={samples_per_pixel}, photometric={photometric_interpretation}, planar_configuration={planar_configuration}"
        ))),
    }
}

/// Sample count of a `width` x `height` image with `channels` samples per
/// pixel, once its size at `bytes_per_sample` passes the decoded-image limit.
fn checked_sample_len(
    width: u32,
    height: u32,
    channels: usize,
    bytes_per_sample: usize,
    what: &str,
) -> Result<usize, String> {
    let bytes = crate::core::limits::checked_product_to_usize(
        &[
            u64::from(width),
            u64::from(height),
            channels as u64,
            bytes_per_sample as u64,
        ],
        crate::core::limits::MAX_DECODED_IMAGE_BYTES,
        what,
    )?;
    Ok(bytes / bytes_per_sample)
}

pub(super) fn decode_rle_lossless_frame(
    frame_bytes: &[u8],
    width: u32,
    height: u32,
    samples_per_pixel: u16,
    photometric_interpretation: &str,
    bit_depth: DicomBitDepth,
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
    let stored_components = match (samples_per_pixel, photometric_interpretation) {
        (3, "RGB") => 3usize,
        (1, "MONOCHROME1" | "MONOCHROME2") => 1usize,
        _ => {
            return Err(WsiError::DisplayConversion(format!(
                "unsupported DICOM RLE pixel format: samples_per_pixel={samples_per_pixel}, photometric={photometric_interpretation}"
            )));
        }
    };
    let bytes_per_sample = usize::from(bit_depth.bytes_per_sample());
    let expected_segments = stored_components * bytes_per_sample;
    if segment_count < expected_segments {
        return Err(WsiError::DisplayConversion(format!(
            "DICOM RLE has {segment_count} segments, expected at least {expected_segments}"
        )));
    }
    let pixel_count_u64 = u64::from(width) * u64::from(height);
    let working_set_bytes = pixel_count_u64
        .saturating_mul(u64::from(samples_per_pixel) + 3)
        .saturating_mul(bytes_per_sample as u64);
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

    let data = match (bit_depth, samples_per_pixel, photometric_interpretation) {
        (DicomBitDepth::Eight, 3, "RGB") => {
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
            CpuTileData::u8(rgb)
        }
        (DicomBitDepth::Eight, 1, "MONOCHROME1" | "MONOCHROME2") => {
            let rgb_len = pixel_count
                .checked_mul(3)
                .ok_or_else(|| WsiError::DisplayConversion("DICOM RLE RGB size overflow".into()))?;
            let mut rgb = try_rle_buffer_with_capacity(rgb_len)?;
            for &gray in &planes[0] {
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
            CpuTileData::u8(rgb)
        }
        (DicomBitDepth::Uint16 { .. }, _, _) => {
            let mask = bit_depth.unsigned_sample_mask();
            let rgb_len = pixel_count.checked_mul(3).ok_or_else(|| {
                WsiError::DisplayConversion("DICOM RLE RGB16 size overflow".into())
            })?;
            let mut rgb = Vec::new();
            rgb.try_reserve_exact(rgb_len).map_err(|_| {
                WsiError::DisplayConversion(format!(
                    "unable to allocate {rgb_len} samples for DICOM RLE16 decode"
                ))
            })?;
            match (samples_per_pixel, photometric_interpretation) {
                (3, "RGB") => {
                    for (idx, &red_high) in planes[0].iter().enumerate() {
                        rgb.push(u16::from_be_bytes([red_high, planes[1][idx]]) & mask);
                        rgb.push(u16::from_be_bytes([planes[2][idx], planes[3][idx]]) & mask);
                        rgb.push(u16::from_be_bytes([planes[4][idx], planes[5][idx]]) & mask);
                    }
                }
                (1, "MONOCHROME1" | "MONOCHROME2") => {
                    for (&high, &low) in planes[0].iter().zip(&planes[1]) {
                        let gray = u16::from_be_bytes([high, low]) & mask;
                        rgb.extend_from_slice(&[gray, gray, gray]);
                    }
                }
                _ => unreachable!("DICOM RLE pixel format was validated before allocation"),
            }
            CpuTileData::u16(rgb)
        }
        _ => unreachable!("DICOM RLE pixel format was validated before allocation"),
    };

    CpuTile::new(
        width,
        height,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        data,
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

/// The error for raw access to a sparse-grid tile that has no stored frame.
pub(super) fn sparse_missing_raw_tile_error(col: i64, row: i64, level: u32) -> WsiError {
    WsiError::Unsupported {
        reason: format!(
            "raw compressed tile access is not available for sparse missing DICOM tile ({col}, {row}) at level {level}"
        ),
    }
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
    crop_rgb_interleaved_buffer(buffer, 0, 0, width, height)
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
        (1, "MONOCHROME1") => Ok(EncodedTilePhotometricInterpretation::Monochrome1),
        (1, "MONOCHROME2") => Ok(EncodedTilePhotometricInterpretation::Monochrome2),
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

/// Black RGB tile in the image's decoded sample type, so sparse gaps composite
/// with decoded 8-bit or 16-bit-word frames.
pub(super) fn black_sample_buffer(
    width: u32,
    height: u32,
    bit_depth: DicomBitDepth,
) -> Result<CpuTile, WsiError> {
    let len = checked_sample_len(
        width,
        height,
        3,
        usize::from(bit_depth.bytes_per_sample()),
        "DICOM black tile",
    )
    .map_err(WsiError::DisplayConversion)?;
    let data = match bit_depth {
        DicomBitDepth::Eight => CpuTileData::u8(vec![0; len]),
        DicomBitDepth::Uint16 { .. } => CpuTileData::u16(vec![0; len]),
    };
    CpuTile::new(
        width,
        height,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        data,
    )
}
