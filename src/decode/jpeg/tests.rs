use super::input::{
    checked_jpeg_preparation_len, patch_sof_dimensions, planned_dimension_patch, repair_jpeg_eoi,
    set_sof_dimensions, try_decode_jpeg_rgb_scaled,
};
use super::*;
use std::borrow::Cow;

fn ensure_jpeg_eoi<'a>(input: &'a [u8]) -> Cow<'a, [u8]> {
    if input.len() >= 2 && input[input.len() - 2..] == [0xFF, 0xD9] {
        return Cow::Borrowed(input);
    }

    let mut repaired = input.to_vec();
    repair_jpeg_eoi(&mut repaired);
    Cow::Owned(repaired)
}

fn patch_jpeg_dimensions<'a>(
    input: &'a [u8],
    expected_width: u32,
    expected_height: u32,
    force_dimensions: bool,
) -> Cow<'a, [u8]> {
    let Some((sof_offset, width, height, force)) =
        planned_dimension_patch(input, expected_width, expected_height, force_dimensions)
    else {
        return Cow::Borrowed(input);
    };

    let mut patched = input.to_vec();
    if force {
        set_sof_dimensions(&mut patched, sof_offset, width, height);
    } else {
        patch_sof_dimensions(&mut patched, sof_offset, width, height);
    }
    Cow::Owned(patched)
}
use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};

fn encode_test_jpeg(img: &image::RgbImage) -> Vec<u8> {
    let mut encoded = Vec::new();
    JpegEncoder::new(&mut encoded, 90)
        .encode(
            img.as_raw().as_slice(),
            img.width() as u16,
            img.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    encoded
}

fn decode_jpeg_rgb(
    data: &[u8],
    tables: Option<&[u8]>,
    expected_width: u32,
    expected_height: u32,
) -> Result<DecodedJpegRgb, WsiError> {
    decode_jpeg_rgb_with_color_transform(
        data,
        tables,
        expected_width,
        expected_height,
        J2kColorTransform::Auto,
    )
}

fn jpeg_tile_geometry(data: &[u8]) -> Result<JpegTileGeometry, WsiError> {
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

struct ParsedJpegTileHeader {
    width: u16,
    height: u16,
    restart_interval: u16,
    max_h: u8,
    max_v: u8,
}

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
            for component in components.as_chunks::<3>().0.iter().take(component_count) {
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

#[test]
fn sof_marker_classification_covers_every_marker_byte() {
    const EXPECTED: [u8; 13] = [
        0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF,
    ];

    for marker in u8::MIN..=u8::MAX {
        assert_eq!(
            is_sof_marker(marker),
            EXPECTED.contains(&marker),
            "unexpected SOF classification for marker FF{marker:02X}",
        );
    }
}

mod batch;
mod cpu;
mod errors;
mod input_repair;
mod lossless;
