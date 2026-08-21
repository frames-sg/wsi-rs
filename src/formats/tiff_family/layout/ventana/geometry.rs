use super::*;

fn exact_tile_extent(
    next_delta: Option<f64>,
    edge_delta: Option<f64>,
    previous_delta: Option<f64>,
    fallback: f64,
) -> f64 {
    [next_delta, edge_delta, previous_delta]
        .into_iter()
        .flatten()
        .find(|delta| *delta > 0.5)
        .unwrap_or(fallback)
}

// ── Stitched level geometry ─────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn ventana_exact_tile_dimensions(
    local_col: i64,
    local_row: i64,
    num_cols: i64,
    num_rows: i64,
    positions: &HashMap<(i64, i64), (f64, f64)>,
    area_width: f64,
    area_height: f64,
    fallback_width: f64,
    fallback_height: f64,
) -> (u32, u32) {
    let Some(&(tile_x, tile_y)) = positions.get(&(local_col, local_row)) else {
        return (
            fallback_width.round().max(1.0).min(u32::MAX as f64) as u32,
            fallback_height.round().max(1.0).min(u32::MAX as f64) as u32,
        );
    };

    let has_next_col = local_col + 1 < num_cols;
    let next_width = if has_next_col {
        positions
            .get(&(local_col + 1, local_row))
            .map(|(next_x, _)| next_x - tile_x)
    } else {
        None
    };
    let previous_width = if local_col > 0 {
        positions
            .get(&(local_col - 1, local_row))
            .map(|(previous_x, _)| tile_x - previous_x)
    } else {
        None
    };
    let width = exact_tile_extent(
        next_width,
        (has_next_col || local_col > 0).then_some(area_width - tile_x),
        previous_width,
        fallback_width,
    );

    let has_next_row = local_row + 1 < num_rows;
    let next_height = if has_next_row {
        positions
            .get(&(local_col, local_row + 1))
            .map(|(_, next_y)| next_y - tile_y)
    } else {
        None
    };
    let previous_height = if local_row > 0 {
        positions
            .get(&(local_col, local_row - 1))
            .map(|(_, previous_y)| tile_y - previous_y)
    } else {
        None
    };
    let height = exact_tile_extent(
        next_height,
        (has_next_row || local_row > 0).then_some(area_height - tile_y),
        previous_height,
        fallback_height,
    );

    (
        width.round().max(1.0).min(u32::MAX as f64) as u32,
        height.round().max(1.0).min(u32::MAX as f64) as u32,
    )
}

pub(super) fn ventana_level0_dimensions(
    bif: &BifInfo,
    tile_width: i64,
    tile_height: i64,
) -> Result<(u64, u64), TiffParseError> {
    // Compatibility level dimensions come from the stitched area model
    // (tile advance plus scanned AOI bounds), not from the exact per-tile extents.
    // Keep exact tile positions for placement, but keep public dimensions aligned
    // with average-overlap geometry whenever the AOI metadata exists.
    if bif.areas.is_empty() && !bif.tiles.is_empty() {
        let min_x = bif
            .tiles
            .iter()
            .map(|tile| tile.x)
            .fold(f64::INFINITY, f64::min);
        let min_y = bif
            .tiles
            .iter()
            .map(|tile| tile.y)
            .fold(f64::INFINITY, f64::min);
        let max_right = bif
            .tiles
            .iter()
            .map(|tile| tile.x + tile.width as f64)
            .fold(f64::NEG_INFINITY, f64::max);
        let max_bottom = bif
            .tiles
            .iter()
            .map(|tile| tile.y + tile.height as f64)
            .fold(f64::NEG_INFINITY, f64::max);
        let width = (max_right - min_x).ceil() as u64;
        let height = (max_bottom - min_y).ceil() as u64;
        if width == 0 || height == 0 {
            return Err(TiffParseError::Structure(
                "Ventana BIF: stitched level-0 dimensions resolved to zero".into(),
            ));
        }
        return Ok((width, height));
    }

    let min_x = bif.areas.iter().map(|area| area.x).min().unwrap_or(0) as f64;
    let min_y = bif.areas.iter().map(|area| area.y).min().unwrap_or(0) as f64;
    let mut max_right = 0.0f64;
    let mut max_bottom = 0.0f64;

    for area in &bif.areas {
        if area.tiles_across <= 0 || area.tiles_down <= 0 {
            continue;
        }
        let right = (area.x as f64 - min_x)
            + (area.tiles_across - 1) as f64 * bif.tile_advance_x
            + tile_width as f64;
        let bottom = (area.y as f64 - min_y)
            + (area.tiles_down - 1) as f64 * bif.tile_advance_y
            + tile_height as f64;
        max_right = max_right.max(right);
        max_bottom = max_bottom.max(bottom);
    }

    let width = max_right.ceil() as u64;
    let height = max_bottom.ceil() as u64;
    if width == 0 || height == 0 {
        return Err(TiffParseError::Structure(
            "Ventana BIF: stitched level-0 dimensions resolved to zero".into(),
        ));
    }
    Ok((width, height))
}

pub(super) fn ventana_public_level_dimensions(
    level0_dims: (u64, u64),
    level_idx: u32,
) -> (u64, u64) {
    let factor = 1u64 << level_idx;
    (
        level0_dims.0.div_ceil(factor),
        level0_dims.1.div_ceil(factor),
    )
}

// ── Tests ───────────────────────────────────────────────────────────
