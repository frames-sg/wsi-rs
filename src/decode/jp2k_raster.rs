use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout};
use crate::decode::jp2k::Jp2kColorSpace;
use crate::decode::jp2k_backend::DecodedInterleavedImage;
use crate::error::WsiError;

const CHROMA_VALUES: usize = 256;

struct YcbcrTables {
    red_from_cr: [i16; CHROMA_VALUES],
    green_from_cb: [i32; CHROMA_VALUES],
    green_from_cr: [i32; CHROMA_VALUES],
    blue_from_cb: [i16; CHROMA_VALUES],
}

const fn round_ratio(numerator: i64, denominator: i64) -> i64 {
    if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        -((-numerator + denominator / 2) / denominator)
    }
}

const fn build_ycbcr_tables() -> YcbcrTables {
    let mut tables = YcbcrTables {
        red_from_cr: [0; CHROMA_VALUES],
        green_from_cb: [0; CHROMA_VALUES],
        green_from_cr: [0; CHROMA_VALUES],
        blue_from_cb: [0; CHROMA_VALUES],
    };
    let mut index = 0;
    while index < CHROMA_VALUES {
        let chroma = index as i64 - 128;
        tables.red_from_cr[index] = round_ratio(1_402 * chroma, 1_000) as i16;
        tables.green_from_cb[index] =
            round_ratio(65_536 * (50_000 - 34_414 * chroma), 100_000) as i32;
        tables.green_from_cr[index] = round_ratio(65_536 * (-71_414 * chroma), 100_000) as i32;
        tables.blue_from_cb[index] = round_ratio(1_772 * chroma, 1_000) as i16;
        index += 1;
    }
    tables
}

// OpenSlide 4.0.1 uses these lookup-table values, including the precomputed
// half-unit in the green Cb term, before combining green in 16-bit fixed point.
static YCBCR_TABLES: YcbcrTables = build_ycbcr_tables();

#[inline]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[inline]
fn ycbcr_to_rgb(yy: u8, cb: u8, cr: u8) -> [u8; 3] {
    let yy = i32::from(yy);
    let cb = usize::from(cb);
    let cr = usize::from(cr);
    [
        clamp_u8(yy + i32::from(YCBCR_TABLES.red_from_cr[cr])),
        clamp_u8(yy + ((YCBCR_TABLES.green_from_cb[cb] + YCBCR_TABLES.green_from_cr[cr]) >> 16)),
        clamp_u8(yy + i32::from(YCBCR_TABLES.blue_from_cb[cb])),
    ]
}

pub(crate) fn interleaved_image_to_sample_buffer(
    image: DecodedInterleavedImage,
) -> Result<CpuTile, WsiError> {
    let expected_len = image
        .width
        .checked_mul(image.height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| WsiError::Jp2k("decoded JP2K image size overflow".into()))?;
    if image.pixels.len() != expected_len {
        return Err(WsiError::Jp2k(format!(
            "unexpected decoded JP2K buffer length: expected {}, found {}",
            expected_len,
            image.pixels.len()
        )));
    }

    let pixels = match image.colorspace {
        Jp2kColorSpace::Rgb => image.pixels,
        Jp2kColorSpace::YCbCr => {
            let mut rgb = vec![0u8; expected_len];
            for (src, dst) in image.pixels.chunks_exact(3).zip(rgb.chunks_exact_mut(3)) {
                let rgb = ycbcr_to_rgb(src[0], src[1], src[2]);
                dst.copy_from_slice(&rgb);
            }
            rgb
        }
    };

    Ok(CpuTile {
        width: image.width as u32,
        height: image.height as u32,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(pixels),
    })
}

pub(crate) fn crop_sample_buffer(
    buffer: CpuTile,
    expected_width: u32,
    expected_height: u32,
) -> Result<CpuTile, WsiError> {
    if expected_width == 0 || expected_height == 0 {
        return Err(WsiError::Jp2k(
            "cropped JP2K dimensions must be non-zero".into(),
        ));
    }
    if expected_width > buffer.width || expected_height > buffer.height {
        return Err(WsiError::Jp2k(format!(
            "decoded JP2K buffer too small to crop: decoded {}x{}, requested {}x{}",
            buffer.width, buffer.height, expected_width, expected_height
        )));
    }
    if expected_width == buffer.width && expected_height == buffer.height {
        return Ok(buffer);
    }
    if buffer.layout != CpuTileLayout::Interleaved {
        return Err(WsiError::Jp2k(format!(
            "unsupported JP2K buffer layout for crop: {:?}",
            buffer.layout
        )));
    }

    let channels = buffer.channels as usize;
    let src_width = buffer.width as usize;
    let dst_width = expected_width as usize;
    let dst_height = expected_height as usize;

    let data = match buffer.data {
        CpuTileData::U8(samples) => {
            let mut cropped = Vec::with_capacity(dst_width * dst_height * channels);
            let src_row_stride = src_width * channels;
            let dst_row_width = dst_width * channels;
            for row in 0..dst_height {
                let start = row * src_row_stride;
                cropped.extend_from_slice(&samples[start..start + dst_row_width]);
            }
            CpuTileData::u8(cropped)
        }
        other => {
            return Err(WsiError::Jp2k(format!(
                "unsupported JP2K sample type for crop: {:?}",
                other.sample_type()
            )))
        }
    };

    Ok(CpuTile {
        width: expected_width,
        height: expected_height,
        channels: buffer.channels,
        color_space: buffer.color_space,
        layout: buffer.layout,
        data,
    })
}

#[cfg(test)]
mod tests;
