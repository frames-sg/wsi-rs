use crate::core::types::ycbcr::ycbcr_to_rgb_u8;
use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout};
use crate::decode::jp2k::Jp2kColorSpace;
use crate::decode::jp2k_backend::DecodedInterleavedImage;
use crate::error::WsiError;

pub(crate) fn interleaved_image_to_sample_buffer(
    image: DecodedInterleavedImage,
) -> Result<CpuTile, WsiError> {
    validate_image_len(&image)?;
    let mut pixels = image.pixels;
    if image.colorspace == Jp2kColorSpace::YCbCr {
        for pixel in pixels.as_chunks_mut::<3>().0 {
            let rgb = ycbcr_to_rgb_u8(pixel[0], pixel[1], pixel[2]);
            pixel.copy_from_slice(&rgb);
        }
    }

    Ok(CpuTile {
        width: image.width as u32,
        height: image.height as u32,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(pixels),
    })
}

fn validate_image_len(image: &DecodedInterleavedImage) -> Result<(), WsiError> {
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

    Ok(())
}

pub(crate) fn interleaved_image_to_cropped_sample_buffer(
    image: DecodedInterleavedImage,
    width: u32,
    height: u32,
) -> Result<CpuTile, WsiError> {
    validate_image_len(&image)?;
    if width == 0 || height == 0 {
        return Err(WsiError::Jp2k(
            "cropped JP2K dimensions must be non-zero".into(),
        ));
    }
    if width as usize > image.width || height as usize > image.height {
        return Err(WsiError::Jp2k(format!(
            "decoded JP2K buffer too small to crop: decoded {}x{}, requested {}x{}",
            image.width, image.height, width, height,
        )));
    }
    if (width as usize, height as usize) == (image.width, image.height) {
        return interleaved_image_to_sample_buffer(image);
    }
    // The validated source dimensions bound the logical output and every row.
    let row_bytes = width as usize * 3;
    let mut pixels = Vec::with_capacity(row_bytes * height as usize);
    for row in image
        .pixels
        .chunks_exact(image.width * 3)
        .take(height as usize)
    {
        if image.colorspace == Jp2kColorSpace::YCbCr {
            for pixel in row[..row_bytes].as_chunks::<3>().0 {
                pixels.extend_from_slice(&ycbcr_to_rgb_u8(pixel[0], pixel[1], pixel[2]));
            }
        } else {
            pixels.extend_from_slice(&row[..row_bytes]);
        }
    }

    Ok(CpuTile {
        width,
        height,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(pixels),
    })
}

#[cfg(test)]
mod tests;
