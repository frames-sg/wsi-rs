use super::*;

// ── Tile layout ────────────────────────────────────────────────────

/// How tiles are organized at a given level.
#[derive(Debug)]
#[non_exhaustive]
pub enum TileLayout {
    /// Regular grid — fixed tile size, row-major.
    Regular {
        tile_width: u32,
        tile_height: u32,
        tiles_across: u64,
        tiles_down: u64,
    },
    /// Per-tile offsets (Ventana BIF, some DICOM).
    /// Geometry follows compatibility tilemap semantics and uses floating-point
    /// tile advances/offsets plus conservative extra-tile expansion.
    Irregular {
        tile_advance: (f64, f64),
        /// Extra tiles to consider around the nominal tilemap region, in the
        /// order `(top, bottom, left, right)`.
        extra_tiles: (u32, u32, u32, u32),
        tiles: HashMap<(i64, i64), TileEntry>,
    },
    /// Entire level is one contiguous image (NDPI giant JPEG).
    /// Backend exposes it as a virtual tile grid.
    WholeLevel {
        width: u64,
        height: u64,
        virtual_tile_width: u32,
        virtual_tile_height: u32,
    },
}

fn floor_div_i128(numerator: i128, denominator: i128) -> i128 {
    debug_assert!(denominator > 0);
    if numerator >= 0 {
        numerator / denominator
    } else {
        -((-numerator + denominator - 1) / denominator)
    }
}

fn ceil_div_i128(numerator: i128, denominator: i128) -> i128 {
    debug_assert!(denominator > 0);
    if numerator >= 0 {
        (numerator + denominator - 1) / denominator
    } else {
        numerator / denominator
    }
}

fn i64_exclusive_tile_bound(count: u64) -> i128 {
    i128::from(count).min(i128::from(i64::MAX) + 1)
}

/// Result of tile intersection computation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TileHit {
    pub col: i64,
    pub row: i64,
    /// Pixel offset where this tile's top-left lands in the output buffer.
    pub dest_x: i64,
    pub dest_y: i64,
    /// Floating-point placement used by irregular tilemaps.
    pub dest_x_f64: f64,
    pub dest_y_f64: f64,
}

impl TileLayout {
    /// Compute which tiles intersect the given pixel region.
    pub fn tiles_for_region(&self, x: i64, y: i64, w: u32, h: u32) -> Vec<TileHit> {
        match self {
            TileLayout::Regular {
                tile_width,
                tile_height,
                tiles_across,
                tiles_down,
            } => {
                if *tile_width == 0 || *tile_height == 0 || *tiles_across == 0 || *tiles_down == 0 {
                    return Vec::new();
                }

                let tw = i128::from(*tile_width);
                let th = i128::from(*tile_height);
                let region_x = i128::from(x);
                let region_y = i128::from(y);
                let region_x2 = region_x + i128::from(w);
                let region_y2 = region_y + i128::from(h);
                let max_col = i64_exclusive_tile_bound(*tiles_across);
                let max_row = i64_exclusive_tile_bound(*tiles_down);
                let start_col = floor_div_i128(region_x, tw).clamp(0, max_col);
                let start_row = floor_div_i128(region_y, th).clamp(0, max_row);
                let end_col = ceil_div_i128(region_x2, tw).clamp(0, max_col);
                let end_row = ceil_div_i128(region_y2, th).clamp(0, max_row);

                let mut hits = Vec::new();
                for row in start_row..end_row {
                    for col in start_col..end_col {
                        let dest_x = col * tw - region_x;
                        let dest_y = row * th - region_y;
                        if let (Ok(col), Ok(row), Ok(dest_x), Ok(dest_y)) = (
                            i64::try_from(col),
                            i64::try_from(row),
                            i64::try_from(dest_x),
                            i64::try_from(dest_y),
                        ) {
                            hits.push(TileHit {
                                col,
                                row,
                                dest_x,
                                dest_y,
                                dest_x_f64: dest_x as f64,
                                dest_y_f64: dest_y as f64,
                            });
                        }
                    }
                }
                hits
            }
            TileLayout::WholeLevel {
                width,
                height,
                virtual_tile_width,
                virtual_tile_height,
            } => {
                if *width == 0
                    || *height == 0
                    || *virtual_tile_width == 0
                    || *virtual_tile_height == 0
                {
                    return Vec::new();
                }

                let vtw = i128::from(*virtual_tile_width);
                let vth = i128::from(*virtual_tile_height);
                let max_col = ceil_div_i128(i128::from(*width), vtw).min(i128::from(i64::MAX) + 1);
                let max_row = ceil_div_i128(i128::from(*height), vth).min(i128::from(i64::MAX) + 1);
                let region_x = i128::from(x);
                let region_y = i128::from(y);
                let region_x2 = region_x + i128::from(w);
                let region_y2 = region_y + i128::from(h);
                let start_col = floor_div_i128(region_x, vtw).clamp(0, max_col);
                let start_row = floor_div_i128(region_y, vth).clamp(0, max_row);
                let end_col = ceil_div_i128(region_x2, vtw).clamp(0, max_col);
                let end_row = ceil_div_i128(region_y2, vth).clamp(0, max_row);

                let mut hits = Vec::new();
                for row in start_row..end_row {
                    for col in start_col..end_col {
                        let dest_x = col * vtw - region_x;
                        let dest_y = row * vth - region_y;
                        if let (Ok(col), Ok(row), Ok(dest_x), Ok(dest_y)) = (
                            i64::try_from(col),
                            i64::try_from(row),
                            i64::try_from(dest_x),
                            i64::try_from(dest_y),
                        ) {
                            hits.push(TileHit {
                                col,
                                row,
                                dest_x,
                                dest_y,
                                dest_x_f64: dest_x as f64,
                                dest_y_f64: dest_y as f64,
                            });
                        }
                    }
                }
                hits
            }
            TileLayout::Irregular {
                tile_advance,
                extra_tiles,
                tiles,
            } => {
                let adv_x = tile_advance.0;
                let adv_y = tile_advance.1;
                if !(adv_x.is_finite() && adv_y.is_finite()) || adv_x <= 0.0 || adv_y <= 0.0 {
                    return Vec::new();
                }

                let (extra_top, extra_bottom, extra_left, extra_right) = *extra_tiles;
                let region_x = x as f64;
                let region_y = y as f64;
                let region_x2 = region_x + w as f64;
                let region_y2 = region_y + h as f64;
                let start_col = (region_x / adv_x) as i64 - i64::from(extra_left);
                let end_col = (region_x2 / adv_x).ceil() as i64 + i64::from(extra_right);
                let start_row = (region_y / adv_y) as i64 - i64::from(extra_top);
                let end_row = (region_y2 / adv_y).ceil() as i64 + i64::from(extra_bottom);
                let mut hits = Vec::new();
                for row in start_row..end_row {
                    for col in start_col..end_col {
                        if let Some(entry) = tiles.get(&(col, row)) {
                            let tile_x = col as f64 * adv_x + entry.offset.0;
                            let tile_y = row as f64 * adv_y + entry.offset.1;
                            let tile_x2 = tile_x + entry.dimensions.0 as f64;
                            let tile_y2 = tile_y + entry.dimensions.1 as f64;

                            if tile_x2 > region_x
                                && tile_x < region_x2
                                && tile_y2 > region_y
                                && tile_y < region_y2
                            {
                                hits.push(TileHit {
                                    col,
                                    row,
                                    dest_x: (tile_x - region_x).round() as i64,
                                    dest_y: (tile_y - region_y).round() as i64,
                                    dest_x_f64: tile_x - region_x,
                                    dest_y_f64: tile_y - region_y,
                                });
                            }
                        }
                    }
                }
                hits
            }
        }
    }
}

/// Per-tile position and size in an Irregular layout.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TileEntry {
    pub offset: (f64, f64),
    pub dimensions: (u32, u32),
    /// For irregular TIFF tile grids (e.g. Ventana BIF), the exact TIFF tile
    /// index to use when reading from tile_offsets/tile_byte_counts arrays.
    /// `None` for regular row-major addressing.
    pub tiff_tile_index: Option<usize>,
}

impl TileEntry {
    pub fn new(offset: (f64, f64), dimensions: (u32, u32)) -> Self {
        Self {
            offset,
            dimensions,
            tiff_tile_index: None,
        }
    }

    pub fn with_tiff_tile_index(mut self, tiff_tile_index: usize) -> Self {
        self.tiff_tile_index = Some(tiff_tile_index);
        self
    }
}
