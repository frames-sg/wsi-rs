use crate::core::types::{Dataset, NativeDisplayMismatch, TileRequest, TileViewRequest};
use crate::{SlideLimits, WsiError};

/// The native tile a raw display request names, once its level has a
/// regular grid with the requested tile size. A tile outside the grid is
/// returned as is, for the tile read to reject with `TileRead`.
pub(crate) fn native_regular_display_tile(
    dataset: &Dataset,
    req: &TileViewRequest,
    context: &str,
) -> Result<TileRequest, WsiError> {
    let (_, _, level) = dataset.level(req.scene, req.series, req.level)?;
    match level.native_display_tile(req) {
        Ok(tile) => Ok(tile),
        Err(NativeDisplayMismatch::OutsideGrid) => Ok(req.source_tile()),
        Err(NativeDisplayMismatch::NotRegular) => Err(WsiError::Unsupported {
            reason: format!("{context} access requires a regular native tile grid"),
        }),
        Err(NativeDisplayMismatch::TileSize {
            native: (tile_width, tile_height),
        }) => Err(WsiError::Unsupported {
            reason: format!(
                "{context} tile size {}x{} does not match native tile size {tile_width}x{tile_height}",
                req.tile_width, req.tile_height
            ),
        }),
    }
}

pub(crate) fn batch_decode_target(limits: SlideLimits, logical_bytes: u64) -> u64 {
    limits
        .batch_chunk_bytes()
        .min(limits.operation_transient_bytes())
        .min(limits.slide_transient_bytes())
        .min(
            limits
                .encoded_unit_bytes()
                .saturating_add(logical_bytes.saturating_mul(2)),
        )
}

pub(crate) fn irregular_extra_tiles(
    offset_x: f64,
    offset_y: f64,
    tile_advance_x: f64,
    tile_advance_y: f64,
    tile_width: f64,
    tile_height: f64,
) -> (u32, u32, u32, u32) {
    let extra_right = if offset_x < 0.0 {
        (-offset_x / tile_advance_x).ceil() as u32
    } else {
        0
    };
    let offset_xr = offset_x + (tile_width - tile_advance_x);
    let extra_left = if offset_xr > 0.0 {
        (offset_xr / tile_advance_x).ceil() as u32
    } else {
        0
    };
    let extra_bottom = if offset_y < 0.0 {
        (-offset_y / tile_advance_y).ceil() as u32
    } else {
        0
    };
    let offset_yr = offset_y + (tile_height - tile_advance_y);
    let extra_top = if offset_yr > 0.0 {
        (offset_yr / tile_advance_y).ceil() as u32
    } else {
        0
    };
    (extra_top, extra_bottom, extra_left, extra_right)
}

#[cfg(test)]
mod tests;
