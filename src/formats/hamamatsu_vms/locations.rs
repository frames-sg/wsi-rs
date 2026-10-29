//! Where VMS tiles are stored.
//!
//! A VMS tile is one restart interval of a large baseline JPEG. Its location
//! is that interval's entropy bytes, framed by a derived per-tile header
//! (SOF set to the tile size, restart interval removed) and EOI. VMS files
//! carry no per-tile index: the optional optimisation file records the first
//! interval of each MCU row, and the reader finds the others by scanning the
//! entropy-coded data for RSTn markers once, without decoding it.

use super::model::{VmsJpeg, VmsLevel};
use super::slide::VmsReader;
use super::*;

/// EOI, the suffix of every tile stream.
static EOI: std::sync::LazyLock<Arc<[u8]>> =
    std::sync::LazyLock::new(|| Arc::from(&[0xFF, 0xD9][..]));

impl VmsJpeg {
    /// The restart interval of tile `tile_index`, a `width` x `height` tile.
    fn tile_unit(
        &self,
        tile_index: usize,
        width: u32,
        height: u32,
    ) -> Result<EncodedUnit, WsiError> {
        let (start, stop) = self.checked_tile_entropy_bounds(tile_index)?;
        // The interval ends with its two-byte RSTn marker, or EOI for the
        // last tile; the suffix replaces it.
        let end = stop
            .checked_sub(2)
            .filter(|end| *end > start)
            .ok_or_else(|| {
                WsiError::Jpeg(format!(
                    "VMS JPEG tile {tile_index} in {} has no entropy bytes before its marker",
                    self.path.display()
                ))
            })?;
        EncodedUnit::builder(
            Arc::clone(&self.location_path),
            self.codec.clone(),
            width,
            height,
        )
        .segment(ByteRange::new(start, end - start))
        .framing(StoredFraming::Prefixed)
        .prefix_suffix(self.tile_prefix(width, height)?, Arc::clone(&EOI))
        .build(self.file_len)
    }
}

impl VmsReader {
    pub(super) fn location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        let tile = self.resolve_tile(req)?;
        if tile.level.scale_denom != 1 {
            return Ok(TileLocation::NotAddressable(
                NotAddressableReason::SyntheticLevel {
                    base_level: self.base_level_of(tile.level)?,
                    factor: tile.level.scale_denom,
                },
            ));
        }
        let unit = tile
            .jpeg
            .tile_unit(tile.tile_index, tile.width, tile.height)?;
        Ok(TileLocation::Stored(StoredTile::new(
            tile.width,
            tile.height,
            vec![unit],
        )?))
    }

    /// The full-resolution level that a DCT-scaled level decodes from.
    fn base_level_of(&self, level: &VmsLevel) -> Result<LevelIdx, WsiError> {
        self.slide
            .levels
            .iter()
            .position(|candidate| {
                candidate.scale_denom == 1 && Arc::ptr_eq(&candidate.jpegs[0], &level.jpegs[0])
            })
            .and_then(|index| u32::try_from(index).ok())
            .map(LevelIdx::new)
            .ok_or_else(|| WsiError::Unsupported {
                reason: format!(
                    "VMS level scaled by {} has no full-resolution level of the same JPEG files",
                    level.scale_denom
                ),
            })
    }
}
