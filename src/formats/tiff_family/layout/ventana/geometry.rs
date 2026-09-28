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
        let width = checked_ventana_dimension(max_right - min_x)?;
        let height = checked_ventana_dimension(max_bottom - min_y)?;
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

    let width = checked_ventana_dimension(max_right)?;
    let height = checked_ventana_dimension(max_bottom)?;
    Ok((width, height))
}

fn checked_ventana_dimension(value: f64) -> Result<u64, TiffParseError> {
    let value = value.ceil();
    if !value.is_finite() || value < 1.0 || value >= u64::MAX as f64 {
        return Err(TiffParseError::Structure(format!(
            "Ventana BIF: stitched level-0 dimension is out of range ({value})"
        )));
    }
    Ok(value as u64)
}

pub(super) fn ventana_public_level_dimensions(
    level0_dims: (u64, u64),
    level_idx: u32,
) -> Result<(u64, u64), TiffParseError> {
    let factor = 1u64.checked_shl(level_idx).ok_or_else(|| {
        TiffParseError::Structure(format!(
            "Ventana BIF: level {level_idx} downsample overflows"
        ))
    })?;
    Ok((
        level0_dims.0.div_ceil(factor),
        level0_dims.1.div_ceil(factor),
    ))
}

/// Stored TIFF geometry of one Ventana pyramid directory.
pub(super) struct VentanaStoredLevel {
    pub(super) width: u64,
    pub(super) height: u64,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
}

/// OpenSlide's BIF tilemap for one level: the level-0 grid with its tile
/// advance and per-area offsets divided by `downsample`. At a reduced level
/// every level-0 cell is a `tile / downsample` subtile of the level's stored
/// TIFF tiles, so a fractional subtile size keeps its exact placement extent.
/// Cells whose subtile starts beyond the stored image are omitted: OpenSlide
/// clips those pixels to transparency, so they paint nothing.
pub(super) fn ventana_tilemap_layout(
    bif: &BifInfo,
    downsample: u32,
    stored: &VentanaStoredLevel,
) -> Result<TileLayout, TiffParseError> {
    if downsample == 0 || stored.tile_width == 0 || stored.tile_height == 0 {
        return Err(TiffParseError::Structure(format!(
            "Ventana BIF: invalid tilemap downsample {downsample} or tile size {}x{}",
            stored.tile_width, stored.tile_height
        )));
    }
    let scale = f64::from(downsample);
    let tile_advance = (bif.tile_advance_x / scale, bif.tile_advance_y / scale);
    let extent = (
        f64::from(stored.tile_width) / scale,
        f64::from(stored.tile_height) / scale,
    );
    let dimensions = (extent.0.ceil() as u32, extent.1.ceil() as u32);
    let fractional = extent.0.fract() != 0.0 || extent.1.fract() != 0.0;
    let stored_tiles = (
        stored.width.div_ceil(u64::from(stored.tile_width)),
        stored.height.div_ceil(u64::from(stored.tile_height)),
    );

    let mut tiles = HashMap::with_capacity(bif.tiles.len());
    let mut extras = (0u32, 0u32, 0u32, 0u32);
    for area in &bif.areas {
        let offset = (
            (area.x as f64 - area.start_col as f64 * bif.tile_advance_x) / scale,
            (area.y as f64 - area.start_row as f64 * bif.tile_advance_y) / scale,
        );
        let (top, bottom, left, right) = irregular_extra_tiles(
            offset.0,
            offset.1,
            tile_advance.0,
            tile_advance.1,
            extent.0,
            extent.1,
        );
        extras = (
            extras.0.max(top),
            extras.1.max(bottom),
            extras.2.max(left),
            extras.3.max(right),
        );
        let end_row = area.start_row.checked_add(area.tiles_down).ok_or_else(|| {
            TiffParseError::Structure("Ventana BIF: tile row range overflows".into())
        })?;
        let end_col = area
            .start_col
            .checked_add(area.tiles_across)
            .ok_or_else(|| {
                TiffParseError::Structure("Ventana BIF: tile column range overflows".into())
            })?;
        for row in area.start_row..end_row {
            for col in area.start_col..end_col {
                if subtile_outside_stored_image(
                    (col, row),
                    downsample,
                    extent,
                    stored,
                    stored_tiles,
                ) {
                    continue;
                }
                let entry = TileEntry::new(offset, dimensions);
                tiles.insert(
                    (col, row),
                    if fractional {
                        entry.with_extent(extent)
                    } else {
                        entry
                    },
                );
            }
        }
    }
    Ok(TileLayout::Irregular {
        tile_advance,
        extra_tiles: extras,
        tiles,
    })
}

/// Whether a cell's subtile starts beyond its stored tile's clipped extent.
/// Cells addressing a stored tile outside the grid stay in the tilemap so the
/// read reports the missing tile, as OpenSlide's does.
fn subtile_outside_stored_image(
    (col, row): (i64, i64),
    downsample: u32,
    extent: (f64, f64),
    stored: &VentanaStoredLevel,
    stored_tiles: (u64, u64),
) -> bool {
    let (Ok(col), Ok(row)) = (u64::try_from(col), u64::try_from(row)) else {
        return false;
    };
    let per_tile = u64::from(downsample);
    let (tile_col, tile_row) = (col / per_tile, row / per_tile);
    if tile_col >= stored_tiles.0 || tile_row >= stored_tiles.1 {
        return false;
    }
    let clipped_width =
        (stored.width - tile_col * u64::from(stored.tile_width)).min(u64::from(stored.tile_width));
    let clipped_height = (stored.height - tile_row * u64::from(stored.tile_height))
        .min(u64::from(stored.tile_height));
    let subtile_x = (col % per_tile) as f64 * extent.0;
    let subtile_y = (row % per_tile) as f64 * extent.1;
    subtile_x >= clipped_width as f64 || subtile_y >= clipped_height as f64
}

// ── Tests ───────────────────────────────────────────────────────────
