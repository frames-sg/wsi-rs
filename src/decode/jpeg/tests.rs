#[cfg(any(feature = "metal", feature = "cuda"))]
use super::device::decode_one_jpeg_pixels;
#[cfg(feature = "metal")]
use super::device::progressive_jpeg_requires_cpu_device_route;
#[cfg(all(feature = "metal", target_os = "macos"))]
use super::device::{
    jpeg_device_batch_attempts_for_test, reset_jpeg_device_batch_attempts_for_test,
};
use super::input::{
    checked_jpeg_preparation_len, ensure_jpeg_eoi, patch_jpeg_dimensions,
    try_decode_jpeg_rgb_scaled,
};
use super::*;
#[cfg(any(feature = "metal", feature = "cuda"))]
use crate::core::types::{DeviceTile, TilePixels};
#[cfg(any(feature = "metal", feature = "cuda"))]
use j2k_core::BackendRequest as J2kBackendRequest;
#[cfg(feature = "metal")]
use j2k_jpeg::{DecodeOptions as J2kDecodeOptions, JpegView as J2kJpegView};
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

fn decode_jpeg(
    data: &[u8],
    tables: Option<&[u8]>,
    expected_width: u32,
    expected_height: u32,
) -> Result<image::RgbaImage, WsiError> {
    let decoded = decode_jpeg_rgb(data, tables, expected_width, expected_height)?;
    let mut rgba = Vec::with_capacity(decoded.pixels.len() / 3 * 4);
    for rgb in decoded.pixels.chunks_exact(3) {
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    Ok(
        image::RgbaImage::from_raw(decoded.width, decoded.height, rgba)
            .expect("decoded RGB samples expand to exact RGBA dimensions"),
    )
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

fn progressive_8x8_jpeg() -> Vec<u8> {
    const HEX: &str = concat!(
            "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a07",
            "0706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb0043010304",
            "0405040509050509140d0b0d141414141414141414141414141414141414141414141414141414141414141414141414",
            "1414141414141414141414141414ffc20011080008000803012200021101031101ffc400150001010000000000000000",
            "0000000000000006ffc4001501010100000000000000000000000000000506ffda000c0301000210031000000188136f",
            "7fffc4001410010000000000000000000000000000000000ffda00080101000105027fffc40014110100000000000000",
            "000000000000000000ffda0008010301013f017fffc40014110100000000000000000000000000000000ffda00080102",
            "01013f017fffc40014100100000000000000000000000000000000ffda0008010100063f027fffc40014100100000000",
            "000000000000000000000000ffda0008010100013f217fffda000c03010002000300000010f7ffc40014110100000000",
            "000000000000000000000000ffda0008010301013f107fffc40014110100000000000000000000000000000000ffda00",
            "08010201013f107fffc40014100100000000000000000000000000000000ffda0008010100013f107fffd9",
        );
    assert_eq!(HEX.len() % 2, 0);
    HEX.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap();
            let low = (pair[1] as char).to_digit(16).unwrap();
            ((high << 4) | low) as u8
        })
        .collect()
}

mod batch;
mod cpu;
#[cfg(any(feature = "metal", feature = "cuda"))]
mod device;
mod errors;
mod input_repair;
