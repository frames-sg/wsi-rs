use std::borrow::Cow;

use crate::error::WsiError;
use j2k_jpeg::{
    ColorTransform as J2kColorTransform, DecodeOptions as J2kDecodeOptions,
    DecodeRequest as J2kJpegDecodeRequest, Decoder as J2kJpegDecoder, Downscale as J2kDownscale,
    JpegError as J2kJpegError, JpegView, PixelFormat as J2kPixelFormat, SofKind as J2kSofKind,
};

#[cfg(test)]
use super::JpegTileGeometry;
use super::{DecodedJpegRgb, ScaledJpegDecode, JPEG_MAX_DIMENSION, MAX_JPEG_DECODE_BYTES};

pub(super) fn prepare_jpeg_input<'a>(
    data: &'a [u8],
    tables: Option<&[u8]>,
    expected_width: u32,
    expected_height: u32,
    force_dimensions: bool,
) -> Cow<'a, [u8]> {
    let input = if let Some(tbl) = tables {
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
        Cow::Owned([&tbl[..tbl_end], &data[data_start..]].concat())
    } else {
        Cow::Borrowed(data)
    };
    let patched = patch_jpeg_dimensions(
        input.as_ref(),
        expected_width,
        expected_height,
        force_dimensions,
    );
    match ensure_jpeg_eoi(patched.as_ref()) {
        Cow::Borrowed(bytes) if tables.is_none() && bytes.as_ptr() == data.as_ptr() => {
            Cow::Borrowed(data)
        }
        Cow::Borrowed(bytes) => Cow::Owned(bytes.to_vec()),
        Cow::Owned(bytes) => Cow::Owned(bytes),
    }
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

fn patch_sof_dimensions(header: &mut [u8], sof_offset: usize, width: u16, height: u16) {
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

fn set_sof_dimensions(header: &mut [u8], sof_offset: usize, width: u16, height: u16) {
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
    );
    validate_j2k_jpeg_output_size(input.as_ref())?;
    let view = JpegView::parse_with_options(
        input.as_ref(),
        J2kDecodeOptions::default().with_color_transform(color_transform),
    )
    .map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let decoder = J2kJpegDecoder::from_view(view).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let (pixels, outcome) = decoder
        .decode_request(J2kJpegDecodeRequest::full(J2kPixelFormat::Rgb8))
        .map_err(|err| WsiError::Jpeg(err.to_string()))?;
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
    );
    validate_j2k_jpeg_output_size(input.as_ref())?;
    let view = JpegView::parse_with_options(
        input.as_ref(),
        J2kDecodeOptions::default().with_color_transform(req.color_transform),
    )
    .map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let decoder = J2kJpegDecoder::from_view(view).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let decode_result = if scale == J2kDownscale::None {
        decoder.decode_request(J2kJpegDecodeRequest::full(J2kPixelFormat::Rgb8))
    } else {
        decoder.decode_request(J2kJpegDecodeRequest::scaled(J2kPixelFormat::Rgb8, scale))
    };
    let (pixels, outcome) = match decode_result {
        Ok(decoded) => decoded,
        Err(err) if should_retry_scaled_jpeg_as_full_decode(&err) => return Ok(None),
        Err(err) => return Err(WsiError::Jpeg(err.to_string())),
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

fn should_retry_scaled_jpeg_as_full_decode(err: &J2kJpegError) -> bool {
    matches!(
        err,
        J2kJpegError::DownscaleUnsupported { .. }
            | J2kJpegError::NotImplemented {
                sof: J2kSofKind::Progressive8
            }
    )
}

pub(crate) fn jpeg_dimensions(data: &[u8]) -> Result<(u32, u32), WsiError> {
    let info = J2kJpegDecoder::inspect(data).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    Ok(info.dimensions)
}

#[cfg(test)]
pub(crate) fn jpeg_tile_geometry(data: &[u8]) -> Result<JpegTileGeometry, WsiError> {
    let header = parse_jpeg_tile_header(data)?;
    let restart_interval = header.restart_interval;
    let mcu_width = u32::from(header.max_h) * 8;
    let mcu_height = u32::from(header.max_v) * 8;
    let mcus_per_row = header.width.div_ceil(mcu_width as u16);
    if restart_interval > mcus_per_row {
        return Err(WsiError::Jpeg(format!(
            "JPEG restart interval {} exceeds MCUs per row {}",
            restart_interval, mcus_per_row
        )));
    }
    if mcus_per_row % restart_interval != 0 {
        return Err(WsiError::Jpeg(
            "JPEG restart interval does not divide MCUs per row".into(),
        ));
    }

    Ok(JpegTileGeometry {
        width: header.width as u32,
        height: header.height as u32,
        tile_width: mcu_width * u32::from(restart_interval),
        tile_height: mcu_height,
    })
}

#[cfg(test)]
struct ParsedJpegTileHeader {
    width: u16,
    height: u16,
    restart_interval: u16,
    max_h: u8,
    max_v: u8,
}

#[cfg(test)]
fn parse_jpeg_tile_header(data: &[u8]) -> Result<ParsedJpegTileHeader, WsiError> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(WsiError::Jpeg("JPEG missing SOI marker".into()));
    }

    let mut i = 2usize;
    let mut width = None;
    let mut height = None;
    let mut restart_interval = None;
    let mut max_h = 1u8;
    let mut max_v = 1u8;

    while i + 1 < data.len() {
        if data[i] != 0xFF {
            return Err(WsiError::Jpeg(format!(
                "expected JPEG marker at byte {i}, found {:02X}",
                data[i]
            )));
        }

        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        if i >= data.len() {
            break;
        }
        let marker = data[i];
        i += 1;

        match marker {
            0xD9 | 0xDA => break,
            0x00 | 0xD0..=0xD7 => continue,
            _ => {}
        }

        if i + 1 >= data.len() {
            return Err(WsiError::Jpeg(format!(
                "truncated JPEG marker length for marker FF{:02X}",
                marker
            )));
        }
        let seg_len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if seg_len < 2 || i + seg_len > data.len() {
            return Err(WsiError::Jpeg(format!(
                "invalid JPEG segment length {} for marker FF{:02X}",
                seg_len, marker
            )));
        }
        let payload = &data[i + 2..i + seg_len];

        if is_sof_marker(marker) {
            if payload.len() < 6 {
                return Err(WsiError::Jpeg("JPEG SOF segment too short".into()));
            }
            height = Some(u16::from_be_bytes([payload[1], payload[2]]));
            width = Some(u16::from_be_bytes([payload[3], payload[4]]));
            let component_count = payload[5] as usize;
            let components = &payload[6..];
            if components.len() < component_count * 3 {
                return Err(WsiError::Jpeg("JPEG SOF component table too short".into()));
            }
            for component in components.chunks_exact(3).take(component_count) {
                let sampling = component[1];
                max_h = max_h.max(sampling >> 4);
                max_v = max_v.max(sampling & 0x0F);
            }
        } else if marker == 0xDD {
            if payload.len() < 2 {
                return Err(WsiError::Jpeg("JPEG DRI segment too short".into()));
            }
            restart_interval = Some(u16::from_be_bytes([payload[0], payload[1]]));
        }

        i += seg_len;
    }

    let width = width.ok_or_else(|| WsiError::Jpeg("JPEG missing SOF marker".into()))?;
    let height = height.ok_or_else(|| WsiError::Jpeg("JPEG missing SOF marker".into()))?;
    let restart_interval = restart_interval.unwrap_or(0);
    if restart_interval == 0 {
        return Err(WsiError::Jpeg("JPEG missing restart markers".into()));
    }

    Ok(ParsedJpegTileHeader {
        width,
        height,
        restart_interval,
        max_h,
        max_v,
    })
}

fn is_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
    )
}

pub(super) fn ensure_jpeg_eoi<'a>(input: &'a [u8]) -> Cow<'a, [u8]> {
    if input.len() >= 2 && input[input.len() - 2..] == [0xFF, 0xD9] {
        return Cow::Borrowed(input);
    }

    let mut repaired = input.to_vec();
    if repaired.len() >= 2 && repaired[repaired.len() - 2] == 0xFF {
        let last = repaired.len() - 1;
        repaired[last] = 0xD9;
    } else {
        repaired.push(0xFF);
        repaired.push(0xD9);
    }
    Cow::Owned(repaired)
}

pub(super) fn validate_j2k_jpeg_output_size(input: &[u8]) -> Result<(), WsiError> {
    inspect_j2k_jpeg_output_size(input).map(|_| ())
}

pub(super) fn inspect_j2k_jpeg_output_size(input: &[u8]) -> Result<(u32, u32), WsiError> {
    let info = J2kJpegDecoder::inspect(input).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    let _ = checked_jpeg_rgb_len(info.dimensions.0, info.dimensions.1)?;
    Ok(info.dimensions)
}

pub(super) fn checked_jpeg_rgb_len(width: u32, height: u32) -> Result<usize, WsiError> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| WsiError::Jpeg("JPEG decode size overflow".into()))?;
    if bytes > MAX_JPEG_DECODE_BYTES {
        return Err(WsiError::Jpeg(format!(
            "JPEG decode size {bytes} bytes exceeds {MAX_JPEG_DECODE_BYTES} byte limit"
        )));
    }
    usize::try_from(bytes).map_err(|_| WsiError::Jpeg("JPEG decode size overflow".into()))
}

pub(super) fn crop_jpeg_rgb_to_expected(
    decoded: DecodedJpegRgb,
    expected_width: u32,
    expected_height: u32,
) -> Result<DecodedJpegRgb, WsiError> {
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
    let pixel_count = u64::from(requested_width)
        .checked_mul(u64::from(requested_height))
        .ok_or_else(|| WsiError::Jpeg("scaled JPEG dimensions overflow".into()))?;
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

pub(super) fn patch_jpeg_dimensions<'a>(
    input: &'a [u8],
    expected_width: u32,
    expected_height: u32,
    force_dimensions: bool,
) -> Cow<'a, [u8]> {
    if expected_width == 0
        || expected_height == 0
        || expected_width > u16::MAX as u32
        || expected_height > u16::MAX as u32
    {
        return Cow::Borrowed(input);
    }

    let Some(sof_offset) = find_sof_position(input) else {
        return Cow::Borrowed(input);
    };

    if sof_offset + 9 > input.len() {
        return Cow::Borrowed(input);
    }

    let encoded_height = u16::from_be_bytes([input[sof_offset + 5], input[sof_offset + 6]]);
    let encoded_width = u16::from_be_bytes([input[sof_offset + 7], input[sof_offset + 8]]);
    let needs_patch = encoded_width == 0
        || encoded_height == 0
        || (force_dimensions
            && (encoded_width != expected_width as u16
                || encoded_height != expected_height as u16));
    if !needs_patch {
        return Cow::Borrowed(input);
    }

    let mut patched = input.to_vec();
    if force_dimensions {
        set_sof_dimensions(
            &mut patched,
            sof_offset,
            expected_width as u16,
            expected_height as u16,
        );
    } else {
        patch_sof_dimensions(
            &mut patched,
            sof_offset,
            expected_width as u16,
            expected_height as u16,
        );
    }
    Cow::Owned(patched)
}
