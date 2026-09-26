//! ETS payload reads, codec dispatch and sparse background tiles.

use std::borrow::Cow;
use std::sync::Arc;

use j2k_core::BackendRequest;

use crate::core::limits::{
    checked_product_to_usize, MAX_COMPRESSED_INPUT_BYTES, MAX_DECODED_IMAGE_BYTES,
};
use crate::core::types::{
    ColorSpace, Compression, CpuTile, CpuTileData, CpuTileLayout,
    EncodedTilePhotometricInterpretation, RawCompressedTile, SampleType,
};
use crate::decode::jp2k::{decode_batch_jp2k, Jp2kDecodeJob};
use crate::error::WsiError;

use super::scene::{EtsScene, EtsTile};

impl EtsScene {
    pub(super) fn decode_tile(
        &self,
        tile: &EtsTile,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        crate::core::batch::exactly_one(
            decode_batch_jp2k(&[self.prepare_tile(tile, backend)?]),
            "Olympus ETS JP2K decode",
        )?
    }

    pub(super) fn prepare_tile(
        &self,
        tile: &EtsTile,
        backend: BackendRequest,
    ) -> Result<Jp2kDecodeJob<'static>, WsiError> {
        Ok(Jp2kDecodeJob {
            data: Cow::Owned(self.read_payload(tile)?),
            ..self.jp2k_job(&[], backend)
        })
    }

    /// Every ETS tile is a full-size RGB codestream; edges are cropped later.
    pub(super) fn jp2k_job<'a>(
        &self,
        payload: &'a [u8],
        backend: BackendRequest,
    ) -> Jp2kDecodeJob<'a> {
        Jp2kDecodeJob {
            data: Cow::Borrowed(payload),
            expected_width: self.levels[0].tile_width,
            expected_height: self.levels[0].tile_height,
            rgb_color_space: true,
            backend,
        }
    }

    pub(super) fn read_payload(&self, tile: &EtsTile) -> Result<Vec<u8>, WsiError> {
        let encoded_len = checked_product_to_usize(
            &[u64::from(tile.byte_count)],
            MAX_COMPRESSED_INPUT_BYTES.min(self.encoded_unit_limit),
            "Olympus ETS tile payload",
        )
        .map_err(WsiError::DisplayConversion)?;
        let mut bytes = vec![0; encoded_len];
        self.file
            .read_exact_at(&mut bytes, tile.offset)
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: self.path.clone(),
            })?;
        Ok(bytes)
    }

    pub(super) fn raw_compressed_tile(
        &self,
        tile: &EtsTile,
    ) -> Result<RawCompressedTile, WsiError> {
        let bits_allocated = match self.sample_type {
            SampleType::Uint8 => 8,
            SampleType::Uint16 => 16,
            SampleType::Float32 => {
                return Err(WsiError::Unsupported {
                    reason: "J2K passthrough does not support floating-point ETS samples".into(),
                })
            }
        };
        let (samples_per_pixel, photometric_interpretation) = match self.samples_per_pixel {
            1 => (1, EncodedTilePhotometricInterpretation::Monochrome2),
            3 => (3, EncodedTilePhotometricInterpretation::Rgb),
            other => {
                return Err(WsiError::Unsupported {
                    reason: format!(
                        "J2K passthrough requires 1 or 3 ETS samples per pixel, got {other}"
                    ),
                })
            }
        };
        Ok(RawCompressedTile::builder(Compression::Jp2kRgb)
            .dimensions(self.levels[0].tile_width, self.levels[0].tile_height)
            .bits_allocated(bits_allocated)
            .samples_per_pixel(samples_per_pixel)
            .photometric_interpretation(photometric_interpretation)
            .data(self.read_payload(tile)?)
            .build()?)
    }

    pub(super) fn background_tile(&self, width: u32, height: u32) -> Result<CpuTile, WsiError> {
        let byte_len = checked_product_to_usize(
            &[u64::from(width), u64::from(height), 3],
            MAX_DECODED_IMAGE_BYTES.min(self.decoded_output_limit),
            "Olympus background tile",
        )
        .map_err(WsiError::DisplayConversion)?;
        let pixel_count = checked_product_to_usize(
            &[u64::from(width), u64::from(height)],
            MAX_DECODED_IMAGE_BYTES.min(self.decoded_output_limit),
            "Olympus background pixel count",
        )
        .map_err(WsiError::DisplayConversion)?;
        let mut bytes = Vec::with_capacity(byte_len);
        let rgb = if self.samples_per_pixel >= 3 && self.background.len() >= 3 {
            [self.background[0], self.background[1], self.background[2]]
        } else {
            let gray = self.background.first().copied().unwrap_or(0);
            [gray, gray, gray]
        };
        for _ in 0..pixel_count {
            bytes.extend_from_slice(&rgb);
        }
        CpuTile::new(
            width,
            height,
            3,
            ColorSpace::Rgb,
            CpuTileLayout::Interleaved,
            CpuTileData::u8(bytes),
        )
    }
}
