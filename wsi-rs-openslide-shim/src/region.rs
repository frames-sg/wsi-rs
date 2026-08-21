use wsi_rs::{Level, TileLayout, WsiError};

pub(crate) fn clear_uncovered_pixels(
    level: &Level,
    origin: (i64, i64),
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
            if opaque_output {
                for pixel in pixels.iter_mut() {
                    *pixel &= 0x00ff_ffff;
                }
                for hit in level
                    .tile_layout
                    .tiles_for_region(origin.0, origin.1, size.0, size.1)
                {
                    let Some(entry) = tiles.get(&(hit.col, hit.row)) else {
                        continue;
                    };
                    mark_opaque_rectangle(
                        pixels,
                        size,
                        hit.dest_x_f64,
                        hit.dest_y_f64,
                        entry.dimensions,
                    );
                }
                for pixel in pixels.iter_mut() {
                    if *pixel & 0xff00_0000 == 0 {
                        *pixel = 0;
                    }
                }
            } else {
                let mut covered = vec![false; expected];
                for hit in level
                    .tile_layout
                    .tiles_for_region(origin.0, origin.1, size.0, size.1)
                {
                    let Some(entry) = tiles.get(&(hit.col, hit.row)) else {
                        continue;
                    };
                    mark_covered_rectangle(
                        &mut covered,
                        size,
                        hit.dest_x_f64,
                        hit.dest_y_f64,
                        entry.dimensions,
                    );
                }
                for (pixel, covered) in pixels.iter_mut().zip(covered) {
                    if !covered {
                        *pixel = 0;
                    }
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

fn mark_covered_rectangle(
    covered: &mut [bool],
    size: (u32, u32),
    dest_x: f64,
    dest_y: f64,
    tile_size: (u32, u32),
) {
    let (x0, y0, x1, y1) = rectangle_bounds(size, dest_x, dest_y, tile_size);
    let width = size.0 as usize;
    for row in covered.chunks_exact_mut(width).take(y1).skip(y0) {
        row[x0..x1].fill(true);
    }
}

#[cfg(test)]
mod tests;
