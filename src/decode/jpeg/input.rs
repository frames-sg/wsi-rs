use std::borrow::Cow;

use crate::core::limits::MAX_COMPRESSED_INPUT_BYTES;
use crate::error::WsiError;
use j2k_jpeg::{
    ColorTransform as J2kColorTransform, DecodeOptions as J2kDecodeOptions,
    DecodeRequest as J2kJpegDecodeRequest, Decoder as J2kJpegDecoder, Downscale as J2kDownscale,
    JpegError as J2kJpegError, JpegView, PixelFormat as J2kPixelFormat, SofKind as J2kSofKind,
};

use super::{
    is_sof_marker, DecodedJpegRgb, ScaledJpegDecode, JPEG_MAX_DIMENSION, MAX_JPEG_DECODE_BYTES,
};

pub(super) fn checked_jpeg_preparation_len(
    data_len: usize,
    tables_len: usize,
) -> Result<usize, WsiError> {
    let requested = data_len
        .checked_add(tables_len)
        .and_then(|len| len.checked_add(2))
        .ok_or(WsiError::ResourceLimit {
            resource: "prepared JPEG input",
            requested: u64::MAX,
            limit: MAX_COMPRESSED_INPUT_BYTES,
        })?;
    // Rust's supported address spaces are at most 64 bits, so every `usize`
    // value has an exact `u64` representation.
    let requested_u64 = requested as u64;
    if requested_u64 > MAX_COMPRESSED_INPUT_BYTES {
        return Err(WsiError::ResourceLimit {
            resource: "prepared JPEG input",
            requested: requested_u64,
            limit: MAX_COMPRESSED_INPUT_BYTES,
        });
    }
    Ok(requested)
}

pub(super) fn prepare_jpeg_input<'a>(
    data: &'a [u8],
    tables: Option<&[u8]>,
    expected_width: u32,
    expected_height: u32,
    force_dimensions: bool,
) -> Result<Cow<'a, [u8]>, WsiError> {
    let capacity = checked_jpeg_preparation_len(data.len(), tables.map_or(0, <[u8]>::len))?;
    let mut input = if let Some(tbl) = tables {
        let tbl_end = if tbl.len() >= 2 && tbl[tbl.len() - 2..] == [0xFF, 0xD9] {
            tbl.len() - 2
        } else {
            tbl.len()
        };
        let data_start = if data.len() >= 2 && data[0..2] == [0xFF, 0xD8] {
            2
        } else {
            0
        };
        let mut joined = try_prepared_jpeg_buffer(capacity)?;
        joined.extend_from_slice(&tbl[..tbl_end]);
        joined.extend_from_slice(&data[data_start..]);
        Cow::Owned(joined)
    } else {
        Cow::Borrowed(data)
    };

    let dimension_patch = planned_dimension_patch(
        input.as_ref(),
        expected_width,
        expected_height,
        force_dimensions,
    );
    let needs_eoi_repair = !input.ends_with(&[0xFF, 0xD9]);
    if (dimension_patch.is_some() || needs_eoi_repair) && matches!(input, Cow::Borrowed(_)) {
        let mut owned = try_prepared_jpeg_buffer(capacity)?;
        owned.extend_from_slice(input.as_ref());
        input = Cow::Owned(owned);
    }

    if let Some((sof_offset, width, height, force)) = dimension_patch {
        let bytes = input.to_mut();
        if force {
            set_sof_dimensions(bytes, sof_offset, width, height);
        } else {
            patch_sof_dimensions(bytes, sof_offset, width, height);
        }
    }

    if needs_eoi_repair {
        repair_jpeg_eoi(input.to_mut());
    }
    Ok(input)
}

fn try_prepared_jpeg_buffer(capacity: usize) -> Result<Vec<u8>, WsiError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "prepared JPEG input",
            requested: capacity as u64,
            limit: MAX_COMPRESSED_INPUT_BYTES,
        })?;
    Ok(bytes)
}

fn find_sof_position(header: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < header.len().saturating_sub(1) {
        if header[i] == 0xFF && is_sof_marker(header[i + 1]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub(super) fn patch_sof_dimensions(header: &mut [u8], sof_offset: usize, width: u16, height: u16) {
    if sof_offset + 9 > header.len() {
        return;
    }
    let y = u16::from_be_bytes([header[sof_offset + 5], header[sof_offset + 6]]);
    let x = u16::from_be_bytes([header[sof_offset + 7], header[sof_offset + 8]]);

    let new_y = if y > JPEG_MAX_DIMENSION || y == 0 {
        height.min(JPEG_MAX_DIMENSION)
    } else {
        y
    };
    let new_x = if x > JPEG_MAX_DIMENSION || x == 0 {
        width.min(JPEG_MAX_DIMENSION)
    } else {
        x
    };

    header[sof_offset + 5..sof_offset + 7].copy_from_slice(&new_y.to_be_bytes());
    header[sof_offset + 7..sof_offset + 9].copy_from_slice(&new_x.to_be_bytes());
}

pub(super) fn set_sof_dimensions(header: &mut [u8], sof_offset: usize, width: u16, height: u16) {
    if sof_offset + 9 > header.len() {
        return;
    }
    header[sof_offset + 5..sof_offset + 7].copy_from_slice(&height.to_be_bytes());
    header[sof_offset + 7..sof_offset + 9].copy_from_slice(&width.to_be_bytes());
}

pub(crate) fn decode_jpeg_rgb_with_color_transform(
    data: &[u8],
    tables: Option<&[u8]>,
    expected_width: u32,
    expected_height: u32,
    color_transform: J2kColorTransform,
) -> Result<DecodedJpegRgb, WsiError> {
    decode_jpeg_rgb_with_color_transform_and_patch(
        data,
        tables,
        expected_width,
        expected_height,
        false,
        color_transform,
    )
}

fn parse_jpeg_view(
    data: &[u8],
    color_transform: J2kColorTransform,
) -> Result<JpegView<'_>, WsiError> {
    let view = JpegView::parse(data).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    // Redundant overrides disable the codec's bounded prepared-plan cache.
    // Keep the default plan when its interpretation already matches the caller.
    let color_transform = effective_jpeg_color_transform(
        view.info().sampling.len(),
        view.info().color_space,
        color_transform,
    );
    if color_transform == J2kColorTransform::Auto {
        return Ok(view);
    }
    drop(view);
    JpegView::parse_with_options(
        data,
        J2kDecodeOptions::default().with_color_transform(color_transform),
    )
    .map_err(|err| WsiError::Jpeg(err.to_string()))
}

pub(super) fn effective_jpeg_color_transform(
    sampling_len: usize,
    color_space: j2k_jpeg::ColorSpace,
    requested: J2kColorTransform,
) -> J2kColorTransform {
    if sampling_len != 3
        || matches!(
            (requested, color_space),
            (J2kColorTransform::Auto, _)
                | (J2kColorTransform::ForceRgb, j2k_jpeg::ColorSpace::Rgb)
                | (J2kColorTransform::ForceYCbCr, j2k_jpeg::ColorSpace::YCbCr)
        )
    {
        J2kColorTransform::Auto
    } else {
        requested
    }
}

pub(super) fn decode_jpeg_rgb_with_color_transform_and_patch(
    data: &[u8],
    tables: Option<&[u8]>,
    expected_width: u32,
    expected_height: u32,
    force_dimensions: bool,
    color_transform: J2kColorTransform,
) -> Result<DecodedJpegRgb, WsiError> {
    let input = prepare_jpeg_input(
        data,
        tables,
        expected_width,
        expected_height,
        force_dimensions,
    )?;
    let view = parse_jpeg_view(input.as_ref(), color_transform)?;
    let _ = checked_jpeg_rgb_len(view.info().dimensions.0, view.info().dimensions.1)?;
    let grayscale = view.info().color_space == j2k_jpeg::ColorSpace::Grayscale;
    let decoder = J2kJpegDecoder::from_view(view).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let (pixels, outcome) = decoder
        .decode_request(J2kJpegDecodeRequest::full(if grayscale {
            J2kPixelFormat::Gray8
        } else {
            J2kPixelFormat::Rgb8
        }))
        .map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let pixels = if grayscale {
        expand_grayscale_to_rgb(pixels)?
    } else {
        pixels
    };
    crop_jpeg_rgb_to_expected(
        DecodedJpegRgb {
            width: outcome.decoded.w,
            height: outcome.decoded.h,
            pixels,
        },
        expected_width,
        expected_height,
    )
}

/// Full-size decode of a 12-bit JPEG to interleaved RGB16 samples in the
/// 0..=4095 range. Grayscale is projected to R=G=B by the codec.
pub(super) fn decode_jpeg_rgb16_with_color_transform(
    data: &[u8],
    tables: Option<&[u8]>,
    expected_width: u32,
    expected_height: u32,
    color_transform: J2kColorTransform,
) -> Result<DecodedJpegRgb<u16>, WsiError> {
    let input = prepare_jpeg_input(data, tables, expected_width, expected_height, false)?;
    let view = parse_jpeg_view(input.as_ref(), color_transform)?;
    let (width, height) = view.info().dimensions;
    let sample_count = checked_jpeg_rgb16_len(width, height)?;
    let decoder = J2kJpegDecoder::from_view(view).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let (bytes, outcome) = decoder
        .decode_request(J2kJpegDecodeRequest::full(J2kPixelFormat::Rgb16))
        .map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let decoded_samples = u64::from(outcome.decoded.w) * u64::from(outcome.decoded.h) * 3;
    if bytes.len() as u64 != decoded_samples * 2 || decoded_samples > sample_count as u64 {
        return Err(WsiError::Jpeg(format!(
            "12-bit JPEG decode produced {} bytes for {}x{} RGB16",
            bytes.len(),
            outcome.decoded.w,
            outcome.decoded.h
        )));
    }
    // The codec writes 16-bit samples little-endian.
    let pixels = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| u16::from_le_bytes([sample[0], sample[1]]))
        .collect();
    crop_jpeg_rgb_to_expected(
        DecodedJpegRgb {
            width: outcome.decoded.w,
            height: outcome.decoded.h,
            pixels,
        },
        expected_width,
        expected_height,
    )
}

/// DCT-scaled decode of a JPEG whose SOF is forced to `image_width` x
/// `image_height`. Like libjpeg's `scale_denom`, the output keeps partial MCUs:
/// each dimension is `ceil(image_dim / denominator)`.
pub(crate) fn decode_jpeg_rgb_downscaled_with_size_override(
    data: &[u8],
    image_width: u32,
    image_height: u32,
    scale: J2kDownscale,
    color_transform: J2kColorTransform,
) -> Result<DecodedJpegRgb, WsiError> {
    if image_width == 0
        || image_height == 0
        || image_width > u32::from(u16::MAX)
        || image_height > u32::from(u16::MAX)
    {
        return Err(WsiError::Jpeg(
            "JPEG size override requires nonzero u16 dimensions".into(),
        ));
    }
    let input = prepare_jpeg_input(data, None, image_width, image_height, true)?;
    let view = parse_jpeg_view(input.as_ref(), color_transform)?;
    let grayscale = view.info().color_space == j2k_jpeg::ColorSpace::Grayscale;
    let decoder = J2kJpegDecoder::from_view(view).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let pixel_format = if grayscale {
        J2kPixelFormat::Gray8
    } else {
        J2kPixelFormat::Rgb8
    };
    let (pixels, _outcome) = decoder
        .decode_request(J2kJpegDecodeRequest::scaled(pixel_format, scale))
        .map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let pixels = if grayscale {
        expand_grayscale_to_rgb(pixels)?
    } else {
        pixels
    };
    let width = image_width.div_ceil(scale.denominator());
    let height = image_height.div_ceil(scale.denominator());
    if pixels.len() != checked_jpeg_rgb_len(width, height)? {
        return Err(WsiError::Jpeg(format!(
            "scaled JPEG decode produced {} bytes, expected {width}x{height} RGB",
            pixels.len()
        )));
    }
    Ok(DecodedJpegRgb {
        width,
        height,
        pixels,
    })
}

pub(super) fn j2k_downscale_for_dimensions(
    expected_width: u32,
    expected_height: u32,
    requested_width: u32,
    requested_height: u32,
) -> Option<J2kDownscale> {
    if expected_width == requested_width && expected_height == requested_height {
        return Some(J2kDownscale::None);
    }
    for (scale, denom) in [
        (J2kDownscale::Half, 2),
        (J2kDownscale::Quarter, 4),
        (J2kDownscale::Eighth, 8),
    ] {
        if expected_width.is_multiple_of(denom)
            && expected_height.is_multiple_of(denom)
            && expected_width / denom == requested_width
            && expected_height / denom == requested_height
        {
            return Some(scale);
        }
    }
    None
}

pub(super) fn try_decode_jpeg_rgb_scaled(
    req: ScaledJpegDecode<'_>,
) -> Result<Option<DecodedJpegRgb>, WsiError> {
    let Some(scale) = j2k_downscale_for_dimensions(
        req.expected_width,
        req.expected_height,
        req.requested_width,
        req.requested_height,
    ) else {
        return Ok(None);
    };

    let input = prepare_jpeg_input(
        req.data,
        req.tables,
        req.expected_width,
        req.expected_height,
        req.force_dimensions,
    )?;
    let view = parse_jpeg_view(input.as_ref(), req.color_transform)?;
    let _ = checked_jpeg_rgb_len(view.info().dimensions.0, view.info().dimensions.1)?;
    let grayscale = view.info().color_space == j2k_jpeg::ColorSpace::Grayscale;
    let decoder = J2kJpegDecoder::from_view(view).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let pixel_format = if grayscale {
        J2kPixelFormat::Gray8
    } else {
        J2kPixelFormat::Rgb8
    };
    let decode_result = if scale == J2kDownscale::None {
        decoder.decode_request(J2kJpegDecodeRequest::full(pixel_format))
    } else {
        decoder.decode_request(J2kJpegDecodeRequest::scaled(pixel_format, scale))
    };
    let (pixels, outcome) = match decode_result {
        Ok(decoded) => decoded,
        Err(
            J2kJpegError::DownscaleUnsupported { .. }
            | J2kJpegError::NotImplemented {
                sof: J2kSofKind::Progressive8,
            },
        ) => return Ok(None),
        Err(err) => return Err(WsiError::Jpeg(err.to_string())),
    };
    let pixels = if grayscale {
        expand_grayscale_to_rgb(pixels)?
    } else {
        pixels
    };
    let decoded = if scale == J2kDownscale::None {
        DecodedJpegRgb {
            width: outcome.decoded.w,
            height: outcome.decoded.h,
            pixels,
        }
    } else {
        DecodedJpegRgb {
            width: req.requested_width,
            height: req.requested_height,
            pixels,
        }
    };
    Ok(Some(crop_jpeg_rgb_to_expected(
        decoded,
        req.requested_width,
        req.requested_height,
    )?))
}

pub(super) fn expand_grayscale_to_rgb(gray: Vec<u8>) -> Result<Vec<u8>, WsiError> {
    let output_len = gray
        .len()
        .checked_mul(3)
        .ok_or_else(|| WsiError::Jpeg("grayscale JPEG RGB expansion overflow".into()))?;
    if output_len as u64 > MAX_JPEG_DECODE_BYTES {
        return Err(WsiError::Jpeg(format!(
            "grayscale JPEG RGB expansion requires {output_len} bytes, exceeding {MAX_JPEG_DECODE_BYTES}"
        )));
    }
    let mut rgb = Vec::new();
    rgb.try_reserve_exact(output_len)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "grayscale JPEG RGB expansion",
            requested: output_len as u64,
            limit: MAX_JPEG_DECODE_BYTES,
        })?;
    for sample in gray {
        rgb.extend_from_slice(&[sample, sample, sample]);
    }
    Ok(rgb)
}

pub(crate) fn jpeg_dimensions(data: &[u8]) -> Result<(u32, u32), WsiError> {
    let info = J2kJpegDecoder::inspect(data).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    Ok(info.dimensions)
}

pub(super) fn repair_jpeg_eoi(input: &mut Vec<u8>) {
    if input.len() >= 2 && input[input.len() - 2] == 0xFF {
        let last = input.len() - 1;
        input[last] = 0xD9;
    } else {
        input.push(0xFF);
        input.push(0xD9);
    }
}

pub(super) fn checked_jpeg_rgb_len(width: u32, height: u32) -> Result<usize, WsiError> {
    // A pair of `u32` dimensions always multiplies exactly in `u64`.
    let pixels = u64::from(width) * u64::from(height);
    let bytes = pixels
        .checked_mul(3)
        .ok_or_else(|| WsiError::Jpeg("JPEG decode size overflow".into()))?;
    if bytes > MAX_JPEG_DECODE_BYTES {
        return Err(WsiError::Jpeg(format!(
            "JPEG decode size {bytes} bytes exceeds {MAX_JPEG_DECODE_BYTES} byte limit"
        )));
    }
    Ok(usize::try_from(bytes).expect("the 512 MiB JPEG limit fits supported usize targets"))
}

/// RGB16 sample count for a decode, bounded by the same byte limit as 8-bit
/// RGB decodes.
fn checked_jpeg_rgb16_len(width: u32, height: u32) -> Result<usize, WsiError> {
    let samples = checked_jpeg_rgb_len(width, height)?;
    let bytes = samples as u64 * 2;
    if bytes > MAX_JPEG_DECODE_BYTES {
        return Err(WsiError::Jpeg(format!(
            "JPEG decode size {bytes} bytes exceeds {MAX_JPEG_DECODE_BYTES} byte limit"
        )));
    }
    Ok(samples)
}

pub(super) fn crop_jpeg_rgb_to_expected<T: Copy>(
    decoded: DecodedJpegRgb<T>,
    expected_width: u32,
    expected_height: u32,
) -> Result<DecodedJpegRgb<T>, WsiError> {
    if expected_width == 0 || expected_height == 0 {
        return Ok(decoded);
    }
    if decoded.width <= expected_width && decoded.height <= expected_height {
        return Ok(decoded);
    }

    let crop_w = decoded.width.min(expected_width) as usize;
    let crop_h = decoded.height.min(expected_height) as usize;
    let src_stride = decoded.width as usize * 3;
    let dst_stride = crop_w * 3;
    let mut cropped = Vec::with_capacity(crop_w * crop_h * 3);
    for row in 0..crop_h {
        let start = row * src_stride;
        cropped.extend_from_slice(&decoded.pixels[start..start + dst_stride]);
    }
    Ok(DecodedJpegRgb {
        width: crop_w as u32,
        height: crop_h as u32,
        pixels: cropped,
    })
}

pub(super) fn resize_jpeg_rgb_nearest(
    decoded: DecodedJpegRgb,
    requested_width: u32,
    requested_height: u32,
) -> Result<DecodedJpegRgb, WsiError> {
    // A pair of `u32` dimensions always multiplies exactly in `u64`.
    let pixel_count = u64::from(requested_width) * u64::from(requested_height);
    let len = pixel_count
        .checked_mul(3)
        .ok_or_else(|| WsiError::Jpeg("scaled JPEG buffer size overflow".into()))?;
    if len > MAX_JPEG_DECODE_BYTES {
        return Err(WsiError::Jpeg(format!(
            "JPEG scaled decode size {len} bytes exceeds {MAX_JPEG_DECODE_BYTES} byte limit"
        )));
    }

    let mut pixels = vec![0u8; len as usize];
    let src_width = decoded.width as usize;
    let src_height = decoded.height as usize;
    let dst_width = requested_width as usize;
    let dst_height = requested_height as usize;
    for y in 0..dst_height {
        let src_y = y * src_height / dst_height;
        for x in 0..dst_width {
            let src_x = x * src_width / dst_width;
            let src = (src_y * src_width + src_x) * 3;
            let dst = (y * dst_width + x) * 3;
            pixels[dst..dst + 3].copy_from_slice(&decoded.pixels[src..src + 3]);
        }
    }

    Ok(DecodedJpegRgb {
        width: requested_width,
        height: requested_height,
        pixels,
    })
}

pub(super) fn planned_dimension_patch(
    input: &[u8],
    expected_width: u32,
    expected_height: u32,
    force_dimensions: bool,
) -> Option<(usize, u16, u16, bool)> {
    if expected_width == 0
        || expected_height == 0
        || expected_width > u16::MAX as u32
        || expected_height > u16::MAX as u32
    {
        return None;
    }

    let sof_offset = find_sof_position(input)?;

    if sof_offset + 9 > input.len() {
        return None;
    }

    let encoded_height = u16::from_be_bytes([input[sof_offset + 5], input[sof_offset + 6]]);
    let encoded_width = u16::from_be_bytes([input[sof_offset + 7], input[sof_offset + 8]]);
    let needs_patch = encoded_width == 0
        || encoded_height == 0
        || (force_dimensions
            && (encoded_width != expected_width as u16
                || encoded_height != expected_height as u16));
    if !needs_patch {
        return None;
    }
    Some((
        sof_offset,
        expected_width as u16,
        expected_height as u16,
        force_dimensions,
    ))
}
