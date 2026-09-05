//! ETS payload reads, codec dispatch and sparse background tiles.

use std::borrow::Cow;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use j2k_core::BackendRequest;

use crate::core::limits::{
    checked_product_to_usize, MAX_COMPRESSED_INPUT_BYTES, MAX_DECODED_IMAGE_BYTES,
};
use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout};
use crate::decode::jp2k::{decode_batch_jp2k, Jp2kDecodeJob};
use crate::error::WsiError;

use super::scene::{EtsScene, EtsTile};

impl EtsScene {
    pub(super) fn decode_tile(
        &self,
        tile: &EtsTile,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let mut file = File::open(&self.path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: self.path.clone(),
        })?;
        file.seek(SeekFrom::Start(tile.offset))?;
        let encoded_len = checked_product_to_usize(
            &[u64::from(tile.byte_count)],
            MAX_COMPRESSED_INPUT_BYTES.min(self.encoded_unit_limit),
            "Olympus ETS tile payload",
        )
        .map_err(WsiError::DisplayConversion)?;
        let mut bytes = vec![0; encoded_len];
        file.read_exact(&mut bytes)?;
        crate::core::batch::exactly_one(
            decode_batch_jp2k(&[Jp2kDecodeJob {
                data: Cow::Owned(bytes),
                expected_width: self.levels[0].tile_width,
                expected_height: self.levels[0].tile_height,
                rgb_color_space: true,
                backend,
            }]),
            "Olympus ETS JP2K decode",
        )?
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
