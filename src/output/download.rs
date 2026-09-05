//! Checked materialization of tightly packed device readback bytes.

use crate::{error::WsiError, CpuTile, CpuTileData, CpuTileLayout, PixelFormat};

pub(super) fn tight_download_layout(
    width: u32,
    height: u32,
    format: PixelFormat,
    backend: &str,
) -> Result<(usize, usize), WsiError> {
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(format.bytes_per_pixel()))
        .ok_or_else(|| {
            WsiError::DisplayConversion(format!("{backend} host download row size overflow"))
        })?;
    let byte_len = usize::try_from(height)
        .ok()
        .and_then(|height| height.checked_mul(row_bytes))
        .ok_or_else(|| {
            WsiError::DisplayConversion(format!("{backend} host download size overflow"))
        })?;
    Ok((row_bytes, byte_len))
}

pub(super) fn downloaded_bytes_to_cpu_tile(
    width: u32,
    height: u32,
    format: PixelFormat,
    bytes: Vec<u8>,
    backend: &str,
) -> Result<CpuTile, WsiError> {
    let (_, expected) = tight_download_layout(width, height, format, backend)?;
    if bytes.len() != expected {
        return Err(WsiError::DisplayConversion(format!(
            "{backend} host download expected {expected} bytes, received {}",
            bytes.len()
        )));
    }
    let data = match format {
        PixelFormat::Rgb8 | PixelFormat::Rgba8 | PixelFormat::Gray8 => CpuTileData::u8(bytes),
        PixelFormat::Rgb16 | PixelFormat::Rgba16 | PixelFormat::Gray16 => {
            let samples = bytes
                .chunks_exact(2)
                .map(|sample| u16::from_ne_bytes([sample[0], sample[1]]))
                .collect();
            CpuTileData::u16(samples)
        }
    };
    CpuTile::new(
        width,
        height,
        format.channels() as u16,
        format.color_space(),
        CpuTileLayout::Interleaved,
        data,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_readback_retains_interleaved_pixels_and_checks_total_extent() {
        let bytes = vec![1, 2, 3, 4, 5, 6];
        let tile = downloaded_bytes_to_cpu_tile(2, 1, PixelFormat::Rgb8, bytes.clone(), "CUDA")
            .expect("valid RGB8 readback");
        let CpuTileData::U8(actual) = tile.data else {
            panic!("RGB8 readback must retain byte storage");
        };
        assert_eq!(&*actual, &bytes);
        assert!(tight_download_layout(u32::MAX, u32::MAX, PixelFormat::Rgba16, "Metal").is_err());
    }

    #[test]
    fn typed_readback_preserves_samples_for_both_backend_labels() {
        for backend in ["CUDA", "Metal"] {
            let samples = [0x1234_u16, 0xabcd, 0, u16::MAX, 1, 256];
            let bytes = samples.into_iter().flat_map(u16::to_ne_bytes).collect();
            let tile = downloaded_bytes_to_cpu_tile(2, 1, PixelFormat::Rgb16, bytes, backend)
                .expect("valid native-endian RGB16 readback");
            let CpuTileData::U16(actual) = tile.data else {
                panic!("RGB16 readback must retain U16 storage");
            };
            assert_eq!(&*actual, &samples);
        }
    }

    #[test]
    fn readback_size_errors_preserve_backend_context() {
        for backend in ["CUDA", "Metal"] {
            for byte_len in [11, 13] {
                let error = downloaded_bytes_to_cpu_tile(
                    2,
                    2,
                    PixelFormat::Rgb8,
                    vec![0; byte_len],
                    backend,
                )
                .expect_err("readback must have exactly the logical byte count");
                let WsiError::DisplayConversion(message) = error else {
                    panic!("readback size errors must remain display conversion errors");
                };
                assert_eq!(
                    message,
                    format!("{backend} host download expected 12 bytes, received {byte_len}")
                );
            }
        }
    }
}
