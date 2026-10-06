use std::collections::HashMap;
use wsi_rs::{ColorSpace, Level, RegionRequest, Slide, TileEntry, TileHit, TileLayout, WsiError};

/// Irregular reads up to one 256x256 viewer tile are composed in one pass.
const SINGLE_PASS_IRREGULAR_PIXELS: u64 = 256 * 256;
/// Larger irregular reads use 32,768-pixel (128 KiB ARGB) bands.
const IRREGULAR_BAND_PIXELS: u64 = 32 * 1024;
/// Other reads use 262,144-pixel (1 MiB ARGB) bands.
const BAND_PIXELS: u64 = 256 * 1024;

/// Bound intermediate color and coverage images while writing the caller's
/// complete destination. Small viewer reads retain a single composition.
pub(crate) fn read_region_into(
    slide: &Slide,
    level: &Level,
    request: &RegionRequest,
    offset: (f64, f64),
    destination: &mut [u32],
) -> Result<(), WsiError> {
    let (width, height) = request.size_px;
    let pixels = u64::from(width) * u64::from(height);
    if pixels != destination.len() as u64 || width == 0 || height == 0 {
        return Err(WsiError::DisplayConversion(
            "region destination must match positive dimensions".into(),
        ));
    }
    let within_limits = pixels <= slide.limits().region_pixels()
        && pixels <= slide.limits().region_rgba_bytes() / 4;
    if within_limits
        && matches!(level.tile_layout, TileLayout::Regular { .. })
        && offset == (0.0, 0.0)
        && slide.read_cached_region_argb32_into(request, offset, destination)?
    {
        clear_outside_level(
            level.dimensions,
            request.origin_px,
            request.size_px,
            destination,
        );
        return Ok(());
    }
    let irregular_tiles = match &level.tile_layout {
        TileLayout::Irregular { tiles, .. } => Some(tiles),
        _ => None,
    };
    let mut bandable = irregular_tiles.is_none();
    if let Some(tiles) = irregular_tiles.filter(|_| within_limits) {
        let hits = level.tile_layout.tiles_for_region(
            request.origin_px.0,
            request.origin_px.1,
            width.saturating_add(1),
            height.saturating_add(1),
        );
        if hits.is_empty() {
            destination.fill(0);
            return Ok(());
        }
        // Resident opaque tiles compose straight into the caller's pixels,
        // without banded intermediate images or a conversion pass. Only
        // whole-pixel hits whose area can cover the region qualify.
        if may_cover_at_whole_pixels(tiles, &hits, offset, (width, height))
            && slide.read_cached_region_argb32_into(request, offset, destination)?
        {
            return Ok(());
        }
        // A rounded-up source can paint beyond its fractional placement
        // extent. Keep that complete clip so a band cannot cull its coverage.
        bandable = hits.iter().all(|hit| {
            tiles.get(&(hit.col, hit.row)).is_some_and(|entry| {
                entry.extent() == (f64::from(entry.dimensions.0), f64::from(entry.dimensions.1))
                    && banding_preserves_filter(
                        (hit.dest_x_f64 - offset.0, hit.dest_y_f64 - offset.1),
                        entry.dimensions,
                        (width, height),
                    )
            })
        });
    }
    // Small viewer reads need one composition. Splitting them into short
    // bands repeats tile planning and can probe source metadata for empty
    // bands in sparse levels. The complete clip is retained when it affects
    // Pixman's sampling.
    let band_pixels = if irregular_tiles.is_some() && pixels > SINGLE_PASS_IRREGULAR_PIXELS {
        IRREGULAR_BAND_PIXELS
    } else {
        BAND_PIXELS
    };
    // Let Slide report its ordinary validation error for an oversized request;
    // splitting must not bypass the limit on the complete output.
    let band_height = if !within_limits || !bandable {
        height
    } else {
        (band_pixels / u64::from(width))
            .max(1)
            .min(u64::from(height)) as u32
    };
    for (index, rows) in destination
        .chunks_mut(width as usize * band_height as usize)
        .enumerate()
    {
        let mut band = request.clone();
        band.origin_px.1 = request
            .origin_px
            .1
            .checked_add(index as i64 * i64::from(band_height))
            .ok_or_else(|| WsiError::DisplayConversion("region band origin overflows".into()))?;
        band.size_px.1 = (rows.len() / width as usize) as u32;
        let tile = slide.read_region_subpixel(&band, offset)?;
        let opaque = !matches!(tile.color_space(), ColorSpace::Rgba);
        crate::pixels::tile_to_premultiplied_argb_into(tile, rows)?;
        clear_uncovered_pixels(level, band.origin_px, offset, band.size_px, rows, opaque)?;
    }
    Ok(())
}

/// A necessary condition for a dense whole-pixel composition, checked from
/// tile-map geometry before the slide plans the region and probes its cache.
fn may_cover_at_whole_pixels(
    tiles: &HashMap<(i64, i64), TileEntry>,
    hits: &[TileHit],
    offset: (f64, f64),
    size: (u32, u32),
) -> bool {
    let mut area = 0_u64;
    for hit in hits {
        let (x, y) = (hit.dest_x_f64 - offset.0, hit.dest_y_f64 - offset.1);
        let Some(entry) = tiles.get(&(hit.col, hit.row)) else {
            return false;
        };
        if x.fract() != 0.0 || y.fract() != 0.0 {
            return false;
        }
        let (x0, y0, x1, y1) = rectangle_bounds(size, x, y, entry.dimensions);
        area += (x1 - x0) as u64 * (y1 - y0) as u64;
    }
    area >= u64::from(size.0) * u64::from(size.1)
}

fn banding_preserves_filter(
    destination: (f64, f64),
    source: (u32, u32),
    output: (u32, u32),
) -> bool {
    // Match Cairo's integer offset plus 16.16 transform decomposition.
    let raster = |value: f64| {
        let integer = (value / 2.0).floor();
        integer - ((-value + integer) * 65_536.0).round_ties_even() / 65_536.0
    };
    let (x, y) = (raster(destination.0), raster(destination.1));
    if x.fract() == 0.0 && y.fract() == 0.0 {
        return true;
    }
    let covered = |dest: f64, source: u32, output: u32| {
        let first = dest.floor().max(0.0);
        let end = (dest + f64::from(source)).ceil().min(f64::from(output));
        first < end
            && (first - dest).floor() >= 0.0
            && (end - 1.0 - dest).floor() + 1.0 < f64::from(source)
    };
    // Vertical cuts must not turn a wide RGB24 filter into Pixman's narrow
    // opaque filter. Horizontal clipping stays fixed in every row band.
    !covered(x, source.0, output.0) || covered(y, source.1, output.1)
}

pub(crate) fn clear_uncovered_pixels(
    level: &Level,
    origin: (i64, i64),
    subpixel_offset: (f64, f64),
    size: (u32, u32),
    pixels: &mut [u32],
    opaque_output: bool,
) -> Result<(), WsiError> {
    let expected = (size.0 as usize)
        .checked_mul(size.1 as usize)
        .ok_or_else(|| WsiError::DisplayConversion("region coverage size overflow".into()))?;
    if pixels.len() != expected {
        return Err(WsiError::DisplayConversion(format!(
            "region coverage has {} pixels, expected {expected}",
            pixels.len()
        )));
    }

    match &level.tile_layout {
        TileLayout::Regular { .. } | TileLayout::WholeLevel { .. } => {
            clear_outside_level(level.dimensions, origin, size, pixels);
            Ok(())
        }
        TileLayout::Irregular { tiles, .. } => {
            // Tilemap composition keeps OpenSlide's coverage in its alpha, so
            // only an opaque single-tile copy needs its footprint marked.
            if !opaque_output {
                return Ok(());
            }
            // Hits are placed from the whole-pixel origin; shift them onto the
            // fractional origin, whose region can reach one pixel further.
            let mut hits = level.tile_layout.tiles_for_region(
                origin.0,
                origin.1,
                size.0.saturating_add(1),
                size.1.saturating_add(1),
            );
            // Dense bands often span adjacent tiles. Prove their coverage
            // from rectangles instead of clearing and restoring every alpha.
            hits.sort_unstable_by(|a, b| a.dest_x_f64.total_cmp(&b.dest_x_f64));
            let mut covered = 0;
            for hit in &hits {
                let Some(entry) = tiles.get(&(hit.col, hit.row)) else {
                    continue;
                };
                let (x0, y0, x1, y1) = rectangle_bounds(
                    size,
                    hit.dest_x_f64 - subpixel_offset.0,
                    hit.dest_y_f64 - subpixel_offset.1,
                    entry.dimensions,
                );
                if y0 == 0 && y1 == size.1 as usize {
                    if x0 > covered {
                        break;
                    }
                    covered = covered.max(x1);
                }
            }
            if covered == size.0 as usize {
                return Ok(());
            }
            for pixel in pixels.iter_mut() {
                *pixel &= 0x00ff_ffff;
            }
            for hit in hits {
                let Some(entry) = tiles.get(&(hit.col, hit.row)) else {
                    continue;
                };
                mark_opaque_rectangle(
                    pixels,
                    size,
                    hit.dest_x_f64 - subpixel_offset.0,
                    hit.dest_y_f64 - subpixel_offset.1,
                    entry.dimensions,
                );
            }
            for pixel in pixels.iter_mut() {
                if *pixel & 0xff00_0000 == 0 {
                    *pixel = 0;
                }
            }
            Ok(())
        }
        _ => {
            clear_outside_level(level.dimensions, origin, size, pixels);
            Ok(())
        }
    }
}

fn clear_outside_level(
    level_size: (u64, u64),
    origin: (i64, i64),
    size: (u32, u32),
    pixels: &mut [u32],
) {
    let x0 = (-i128::from(origin.0)).clamp(0, i128::from(size.0)) as usize;
    let y0 = (-i128::from(origin.1)).clamp(0, i128::from(size.1)) as usize;
    let x1 =
        (i128::from(level_size.0) - i128::from(origin.0)).clamp(0, i128::from(size.0)) as usize;
    let y1 =
        (i128::from(level_size.1) - i128::from(origin.1)).clamp(0, i128::from(size.1)) as usize;
    let width = size.0 as usize;
    if x0 == 0 && y0 == 0 && x1 == width && y1 == size.1 as usize {
        return;
    }

    for (row_index, row) in pixels.chunks_exact_mut(width).enumerate() {
        if row_index < y0 || row_index >= y1 || x0 >= x1 {
            row.fill(0);
        } else {
            row[..x0].fill(0);
            row[x1..].fill(0);
        }
    }
}

fn rectangle_bounds(
    size: (u32, u32),
    dest_x: f64,
    dest_y: f64,
    tile_size: (u32, u32),
) -> (usize, usize, usize, usize) {
    let x0 = dest_x.floor().max(0.0).min(f64::from(size.0)) as usize;
    let y0 = dest_y.floor().max(0.0).min(f64::from(size.1)) as usize;
    let x1 = (dest_x + f64::from(tile_size.0))
        .ceil()
        .max(0.0)
        .min(f64::from(size.0)) as usize;
    let y1 = (dest_y + f64::from(tile_size.1))
        .ceil()
        .max(0.0)
        .min(f64::from(size.1)) as usize;
    (x0, y0, x1, y1)
}

fn mark_opaque_rectangle(
    pixels: &mut [u32],
    size: (u32, u32),
    dest_x: f64,
    dest_y: f64,
    tile_size: (u32, u32),
) {
    let (x0, y0, x1, y1) = rectangle_bounds(size, dest_x, dest_y, tile_size);
    let width = size.0 as usize;
    for row in pixels.chunks_exact_mut(width).take(y1).skip(y0) {
        for pixel in &mut row[x0..x1] {
            *pixel |= 0xff00_0000;
        }
    }
}

#[cfg(test)]
mod tests;
