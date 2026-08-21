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

#[derive(Clone, Copy)]
struct GridRegion {
    x: i64,
    y: i64,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy)]
struct GridTileBounds {
    tile_width: i128,
    tile_height: i128,
    max_col: i128,
    max_row: i128,
}

fn grid_tiles_for_region(region: GridRegion, bounds: GridTileBounds) -> Vec<TileHit> {
    let region_x = i128::from(region.x);
    let region_y = i128::from(region.y);
    let region_x2 = region_x + i128::from(region.width);
    let region_y2 = region_y + i128::from(region.height);
    let start_col = floor_div_i128(region_x, bounds.tile_width).clamp(0, bounds.max_col);
    let start_row = floor_div_i128(region_y, bounds.tile_height).clamp(0, bounds.max_row);
    let end_col = ceil_div_i128(region_x2, bounds.tile_width).clamp(0, bounds.max_col);
    let end_row = ceil_div_i128(region_y2, bounds.tile_height).clamp(0, bounds.max_row);

    let mut hits = Vec::new();
    for row in start_row..end_row {
        for col in start_col..end_col {
            let dest_x = col * bounds.tile_width - region_x;
            let dest_y = row * bounds.tile_height - region_y;
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
                    cairo_fixed_dest: None,
                });
            }
        }
    }
    hits
}

fn fractional_grid_tiles_for_region(
    origin: (f64, f64),
    size: (u32, u32),
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
) -> Vec<TileHit> {
    let (x, y) = origin;
    let (width, height) = size;
    if !x.is_finite()
        || !y.is_finite()
        || tile_width == 0
        || tile_height == 0
        || tiles_across == 0
        || tiles_down == 0
    {
        return Vec::new();
    }

    let tile_width = f64::from(tile_width);
    let tile_height = f64::from(tile_height);
    let start_col = (x / tile_width).floor().max(0.0) as u64;
    let start_row = (y / tile_height).floor().max(0.0) as u64;
    let end_col = ((x + f64::from(width)) / tile_width).ceil().max(0.0) as u64;
    let end_row = ((y + f64::from(height)) / tile_height).ceil().max(0.0) as u64;
    let offset_x = x - start_col as f64 * tile_width;
    let offset_y = y - start_row as f64 * tile_height;
    let mut hits = Vec::new();
    for row in (start_row.min(tiles_down)..end_row.min(tiles_down)).rev() {
        for col in (start_col.min(tiles_across)..end_col.min(tiles_across)).rev() {
            // Preserve OpenSlide's operation order: its grid first computes
            // the offset within the starting tile, then advances relative to
            // that tile. The mathematically equivalent `col * width - x`
            // can cross a 16.16 rounding boundary after floating cancellation.
            let dest_x_f64 = (col - start_col) as f64 * tile_width - offset_x;
            let dest_y_f64 = (row - start_row) as f64 * tile_height - offset_y;
            let (Ok(col), Ok(row)) = (i64::try_from(col), i64::try_from(row)) else {
                continue;
            };
            hits.push(TileHit {
                col,
                row,
                dest_x: dest_x_f64.round() as i64,
                dest_y: dest_y_f64.round() as i64,
                dest_x_f64,
                dest_y_f64,
                cairo_fixed_dest: Some((
                    cairo_bilinear_destination(dest_x_f64),
                    cairo_bilinear_destination(dest_y_f64),
                )),
            });
        }
    }
    hits
}

fn cairo_bilinear_destination(destination: f64) -> f64 {
    // Cairo splits a non-integral translation between Pixman's integer source
    // offset and a 16.16 transform. Reproducing that split per tile preserves
    // translation invariance that would be lost by rounding the full origin.
    let integer_offset = (destination / 2.0).floor();
    let transform = ((-destination + integer_offset) * 65_536.0).round_ties_even() / 65_536.0;
    integer_offset - transform
}

fn irregular_tiles_for_fractional_region(
    tile_advance: (f64, f64),
    extra_tiles: (u32, u32, u32, u32),
    tiles: &HashMap<(i64, i64), TileEntry>,
    x: f64,
    y: f64,
    width: u32,
    height: u32,
) -> Vec<TileHit> {
    let (adv_x, adv_y) = tile_advance;
    if !(x.is_finite() && y.is_finite() && adv_x.is_finite() && adv_y.is_finite())
        || adv_x <= 0.0
        || adv_y <= 0.0
    {
        return Vec::new();
    }

    let (extra_top, extra_bottom, extra_left, extra_right) = extra_tiles;
    let region_x2 = x + f64::from(width);
    let region_y2 = y + f64::from(height);
    let start_col = (x / adv_x) as i64 - i64::from(extra_left);
    let end_col = (region_x2 / adv_x).ceil() as i64 + i64::from(extra_right);
    let start_row = (y / adv_y) as i64 - i64::from(extra_top);
    let end_row = (region_y2 / adv_y).ceil() as i64 + i64::from(extra_bottom);
    let mut hits = Vec::new();
    for row in start_row..end_row {
        for col in start_col..end_col {
            let Some(entry) = tiles.get(&(col, row)) else {
                continue;
            };
            let tile_x = col as f64 * adv_x + entry.offset.0;
            let tile_y = row as f64 * adv_y + entry.offset.1;
            let tile_x2 = tile_x + f64::from(entry.dimensions.0);
            let tile_y2 = tile_y + f64::from(entry.dimensions.1);

            if tile_x2 > x && tile_x < region_x2 && tile_y2 > y && tile_y < region_y2 {
                let dest_x_f64 = tile_x - x;
                let dest_y_f64 = tile_y - y;
                hits.push(TileHit {
                    col,
                    row,
                    dest_x: dest_x_f64.round() as i64,
                    dest_y: dest_y_f64.round() as i64,
                    dest_x_f64,
                    dest_y_f64,
                    cairo_fixed_dest: None,
                });
            }
        }
    }
    hits
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
    /// Cairo's fixed-point raster position for regular fractional grids.
    pub(crate) cairo_fixed_dest: Option<(f64, f64)>,
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
                let max_col = i64_exclusive_tile_bound(*tiles_across);
                let max_row = i64_exclusive_tile_bound(*tiles_down);
                grid_tiles_for_region(
                    GridRegion {
                        x,
                        y,
                        width: w,
                        height: h,
                    },
                    GridTileBounds {
                        tile_width: tw,
                        tile_height: th,
                        max_col,
                        max_row,
                    },
                )
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
                grid_tiles_for_region(
                    GridRegion {
                        x,
                        y,
                        width: w,
                        height: h,
                    },
                    GridTileBounds {
                        tile_width: vtw,
                        tile_height: vth,
                        max_col,
                        max_row,
                    },
                )
            }
            TileLayout::Irregular {
                tile_advance,
                extra_tiles,
                tiles,
            } => irregular_tiles_for_fractional_region(
                *tile_advance,
                *extra_tiles,
                tiles,
                x as f64,
                y as f64,
                w,
                h,
            ),
        }
    }

    pub(crate) fn tiles_for_fractional_region(
        &self,
        x: f64,
        y: f64,
        w: u32,
        h: u32,
    ) -> Vec<TileHit> {
        match self {
            TileLayout::Regular {
                tile_width,
                tile_height,
                tiles_across,
                tiles_down,
            } => fractional_grid_tiles_for_region(
                (x, y),
                (w, h),
                *tile_width,
                *tile_height,
                *tiles_across,
                *tiles_down,
            ),
            TileLayout::WholeLevel {
                width,
                height,
                virtual_tile_width,
                virtual_tile_height,
            } => fractional_grid_tiles_for_region(
                (x, y),
                (w, h),
                *virtual_tile_width,
                *virtual_tile_height,
                width.div_ceil(u64::from(*virtual_tile_width)),
                height.div_ceil(u64::from(*virtual_tile_height)),
            ),
            TileLayout::Irregular {
                tile_advance,
                extra_tiles,
                tiles,
            } => irregular_tiles_for_fractional_region(
                *tile_advance,
                *extra_tiles,
                tiles,
                x,
                y,
                w,
                h,
            ),
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
