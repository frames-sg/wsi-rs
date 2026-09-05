//! Shared bounded composition for CZI tiles and small whole-level reads.
use super::raster::{
    bitmap_to_sample_buffer, blit_raw_uncompressed_rgb_subblock, blit_rgb_sample, blit_tile,
    rgb_u8_tile, RgbSample,
};
use super::subblock::bitmap_from_raw_subblock;
use super::*;

impl ZeissSlide {
    pub(super) fn compose_subblocks(
        &self,
        subblocks: &[czi_rs::DirectorySubBlockInfo],
        size: (u32, u32),
        origin: (i32, i32),
        ratio: i32,
    ) -> Result<CpuTile, WsiError> {
        let first = subblocks.first().ok_or_else(|| {
            WsiError::DisplayConversion("CZI composition has no source subblocks".into())
        })?;
        #[cfg(test)]
        let direct = subblocks.iter().all(|info| {
            info.compression == CziCompressionMode::UnCompressed
                && matches!(info.pixel_type, CziPixelType::Bgr24 | CziPixelType::Bgra32)
        });
        let rgb_output = subblocks
            .iter()
            .all(|info| matches!(info.pixel_type, CziPixelType::Bgr24 | CziPixelType::Bgra32));
        let bytes_per_pixel = if rgb_output {
            3
        } else {
            first.pixel_type.bytes_per_pixel() as u64
        };
        let len = checked_product_to_usize(
            &[u64::from(size.0), u64::from(size.1), bytes_per_pixel],
            self.limits
                .decoded_output_bytes()
                .min(MAX_DECODED_IMAGE_BYTES),
            "Zeiss composed level",
        )
        .map_err(WsiError::DisplayConversion)?;
        let mut destination = if rgb_output {
            None
        } else {
            Some(
                czi_rs::Bitmap::zeros(first.pixel_type, size.0, size.1)
                    .map_err(|e| WsiError::DisplayConversion(e.to_string()))?,
            )
        };
        let mut rgb = if rgb_output { vec![0; len] } else { Vec::new() };
        // Holding the file seek lock only for I/O lets decoding run independently.
        let mut ordered: Vec<_> = subblocks.iter().collect();
        ordered.sort_by_key(|info| (info.m_index.unwrap_or(i32::MIN), info.file_position));
        for info in ordered {
            let offset = |value: i32, base: i32, tile: i32| {
                i32::try_from(
                    (i64::from(value) - i64::from(base)).div_euclid(i64::from(ratio))
                        - i64::from(tile),
                )
                .map_err(|_| WsiError::DisplayConversion("CZI subblock offset overflow".into()))
            };
            let x = offset(info.rect.x, self.subblock_origin.0, origin.0)?;
            let y = offset(info.rect.y, self.subblock_origin.1, origin.1)?;
            if let Some(destination) = &mut destination {
                let raw = self.read_source_subblock(info)?;
                #[cfg(test)]
                self.subblock_decodes.fetch_add(1, Ordering::Relaxed);
                let bitmap = bitmap_from_raw_subblock(&raw, self.limits)?;
                blit_tile(destination, &bitmap, x, y)?;
            } else if info.compression == CziCompressionMode::UnCompressed {
                let raw = self.read_source_subblock(info)?;
                blit_raw_uncompressed_rgb_subblock(&mut rgb, size.0, size.1, &raw, x, y)?;
            } else {
                let tile = self.decoded_subblock(info)?;
                let data = tile.data.as_u8().ok_or_else(|| {
                    WsiError::DisplayConversion("CZI RGB composition requires 8-bit samples".into())
                })?;
                blit_rgb_sample(
                    &mut rgb,
                    size,
                    RgbSample {
                        width: tile.width,
                        height: tile.height,
                        data,
                    },
                    (x, y),
                )?;
            }
        }
        if let Some(destination) = destination {
            bitmap_to_sample_buffer(destination)
        } else {
            #[cfg(test)]
            if direct {
                super::slide::ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS.fetch_add(1, Ordering::Relaxed);
            }
            rgb_u8_tile(size.0, size.1, rgb)
        }
    }
}
