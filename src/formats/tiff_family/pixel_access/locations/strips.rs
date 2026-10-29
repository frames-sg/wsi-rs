//! Tile locations of strip-based TIFF levels: one unit per strip, and per
//! sample plane, that the tile's rows fall in.

use super::*;

impl TiffPixelReader {
    /// Each unit is a whole strip, `image_width` by its row count (the last
    /// strip holds the remaining rows); its source region selects the tile's
    /// columns and the rows inside that strip.
    pub(super) fn stripped_level_location(
        &self,
        req: &TileRequest,
        source: &StrippedLevelSource<'_>,
        params: &SourceParams,
    ) -> Result<TileLocation, WsiError> {
        let SourceParams::Units(codec) = params else {
            return Err(WsiError::tile_read(
                req,
                "stripped TIFF source has NDPI parameters",
            ));
        };
        let rows_per_strip = source.rows_per_strip;
        if rows_per_strip == 0 {
            return Err(WsiError::tile_read(
                req,
                "generic stripped TIFF level has zero rows per strip",
            ));
        }
        let window = self.stripped_level_window(req)?;
        let strips_per_plane = window.image_height.div_ceil(rows_per_strip) as usize;
        let window_bottom = window.y + window.height;
        let mut units = Vec::new();
        for strip_row in window.y / rows_per_strip..=(window_bottom - 1) / rows_per_strip {
            let strip_y = strip_row * rows_per_strip;
            let strip_height = rows_per_strip.min(window.image_height - strip_y);
            let first_row = window.y.max(strip_y);
            let end_row = window_bottom.min(strip_y + strip_height);
            let source_region = PixelRect::new(
                window.x,
                first_row - strip_y,
                window.width,
                end_row - first_row,
            );
            let tile_region =
                PixelRect::new(0, first_row - window.y, window.width, end_row - first_row);
            for plane in 0..codec.planes as usize {
                let index = plane * strips_per_plane + strip_row as usize;
                let (offset, byte_count) = source.strip(index)?;
                if byte_count == 0 {
                    return Err(WsiError::tile_read(
                        req,
                        format!("generic stripped TIFF strip {index} is empty"),
                    ));
                }
                units.push(self.stored_unit(
                    req,
                    codec,
                    (window.image_width, strip_height),
                    ByteRange::new(offset, byte_count),
                    source_region,
                    tile_region,
                )?);
            }
        }
        Ok(TileLocation::Stored(StoredTile::new(
            window.width,
            window.height,
            units,
        )?))
    }
}
