use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout};
use crate::decode::jp2k::Jp2kColorSpace;
use crate::decode::jp2k_backend::DecodedInterleavedImage;
use crate::decode::jp2k_raster::{crop_sample_buffer, interleaved_image_to_sample_buffer};
use crate::error::WsiError;

pub(super) fn sample_buffer_from_rgb8_bytes(
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
) -> Result<CpuTile, WsiError> {
    if colorspace == Jp2kColorSpace::Rgb && width == expected_width && height == expected_height {
        let expected_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(|| WsiError::Jp2k("decoded JP2K image size overflow".into()))?;
        if bytes.len() != expected_len {
            return Err(WsiError::Jp2k(format!(
                "unexpected decoded JP2K buffer length: expected {}, found {}",
                expected_len,
                bytes.len()
            )));
        }
        return Ok(CpuTile {
            width,
            height,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(bytes),
        });
    }
    crop_sample_buffer(
        interleaved_image_to_sample_buffer(DecodedInterleavedImage {
            width: width as usize,
            height: height as usize,
            colorspace,
            pixels: bytes,
        })?,
        expected_width,
        expected_height,
    )
}
