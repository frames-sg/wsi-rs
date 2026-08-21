use std::sync::Arc;

use crate::core::registry::composition::region::CompositionShape;
use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout, TileHit};
use crate::error::WsiError;

pub(super) fn blit_integral_samples<T: Copy>(
    out: &mut [T],
    tile_data: &[T],
    tile: &CpuTile,
    hit: &TileHit,
    shape: CompositionShape,
) {
    let tile_width = i64::from(tile.width);
    let tile_height = i64::from(tile.height);
    let src_x = 0i64.max(-hit.dest_x) as usize;
    let src_y = 0i64.max(-hit.dest_y) as usize;
    let dest_x = 0i64.max(hit.dest_x) as usize;
    let dest_y = 0i64.max(hit.dest_y) as usize;
    let copy_width = ((tile_width - src_x as i64) as usize).min(shape.width - dest_x);
    let copy_height = ((tile_height - src_y as i64) as usize).min(shape.height - dest_y);
    let tile_row_stride = tile.width as usize * shape.channels;
    let out_row_stride = shape.width * shape.channels;

    for row in 0..copy_height {
        let src_offset = (src_y + row) * tile_row_stride + src_x * shape.channels;
        let dest_offset = (dest_y + row) * out_row_stride + dest_x * shape.channels;
        let len = copy_width * shape.channels;
        out[dest_offset..dest_offset + len]
            .copy_from_slice(&tile_data[src_offset..src_offset + len]);
    }
}

pub(super) fn mark_integral_tile_opaque(
    alpha: &mut [f32],
    tile: &CpuTile,
    hit: &TileHit,
    shape: CompositionShape,
) {
    let tile_width = i64::from(tile.width);
    let tile_height = i64::from(tile.height);
    let src_x = 0i64.max(-hit.dest_x) as usize;
    let src_y = 0i64.max(-hit.dest_y) as usize;
    let dest_x = 0i64.max(hit.dest_x) as usize;
    let dest_y = 0i64.max(hit.dest_y) as usize;
    let copy_width = ((tile_width - src_x as i64) as usize).min(shape.width - dest_x);
    let copy_height = ((tile_height - src_y as i64) as usize).min(shape.height - dest_y);

    for row in 0..copy_height {
        let dest_offset = (dest_y + row) * shape.width + dest_x;
        alpha[dest_offset..dest_offset + copy_width].fill(1.0);
    }
}

pub(super) fn is_integral_hit(hit: &TileHit) -> bool {
    hit.cairo_fixed_dest.is_none()
        && (hit.dest_x_f64 - hit.dest_x as f64).abs() <= 1e-6
        && (hit.dest_y_f64 - hit.dest_y as f64).abs() <= 1e-6
}

pub(super) fn hit_covers_output(hit: &TileHit, tile: &CpuTile, width: u32, height: u32) -> bool {
    is_integral_hit(hit)
        && hit.dest_x == 0
        && hit.dest_y == 0
        && tile.width == width
        && tile.height == height
}

pub(super) struct DenseIntegralU8Hit<'a> {
    pub(super) hit: &'a TileHit,
    pub(super) data: &'a [u8],
    pub(super) width: i64,
    pub(super) height: i64,
    pub(super) row_stride: usize,
}

pub(super) fn try_compose_dense_integral_u8_region(
    hits: &[TileHit],
    hit_tiles: &[Arc<CpuTile>],
    width: u32,
    height: u32,
    channels: u16,
    color_space: &ColorSpace,
    layout: CpuTileLayout,
) -> Result<Option<CpuTile>, WsiError> {
    if hits.len() != hit_tiles.len() || hits.is_empty() {
        return Ok(None);
    }

    let shape = CompositionShape {
        width: width as usize,
        height: height as usize,
        channels: usize::from(channels),
    };
    let total_samples = shape
        .width
        .checked_mul(shape.height)
        .and_then(|pixels| pixels.checked_mul(shape.channels))
        .ok_or_else(|| WsiError::DisplayConversion("region output size overflow".into()))?;
    let Some(dense_hits) = collect_dense_integral_u8_hits(
        hits,
        hit_tiles,
        channels,
        color_space,
        layout,
        shape.channels,
    )?
    else {
        return Ok(None);
    };
    let Some(out) = compose_dense_integral_u8_rows(&dense_hits, shape, total_samples)? else {
        return Ok(None);
    };

    Ok(Some(CpuTile {
        width,
        height,
        channels,
        color_space: color_space.clone(),
        layout,
        data: CpuTileData::u8(out),
    }))
}

fn collect_dense_integral_u8_hits<'a>(
    hits: &'a [TileHit],
    hit_tiles: &'a [Arc<CpuTile>],
    channels: u16,
    color_space: &ColorSpace,
    layout: CpuTileLayout,
    channel_count: usize,
) -> Result<Option<Vec<DenseIntegralU8Hit<'a>>>, WsiError> {
    let mut dense_hits = Vec::with_capacity(hits.len());
    for (hit, tile) in hits.iter().zip(hit_tiles) {
        if !is_integral_hit(hit)
            || tile.layout != layout
            || tile.channels != channels
            || tile.color_space != *color_space
        {
            return Ok(None);
        }
        let Some(data) = tile.data.as_u8() else {
            return Ok(None);
        };
        let row_stride = (tile.width as usize)
            .checked_mul(channel_count)
            .ok_or_else(|| WsiError::DisplayConversion("tile row stride overflow".into()))?;
        dense_hits.push(DenseIntegralU8Hit {
            hit,
            data,
            width: i64::from(tile.width),
            height: i64::from(tile.height),
            row_stride,
        });
    }
    dense_hits.sort_by_key(|entry| (entry.hit.dest_y, entry.hit.dest_x));
    Ok(Some(dense_hits))
}

pub(super) fn compose_dense_integral_u8_rows(
    dense_hits: &[DenseIntegralU8Hit<'_>],
    shape: CompositionShape,
    total_samples: usize,
) -> Result<Option<Vec<u8>>, WsiError> {
    let out_width_i64 = shape.width as i64;
    let mut out = Vec::with_capacity(total_samples);
    for dst_y in 0..shape.height {
        let dst_y_i64 = dst_y as i64;
        let mut cursor = 0usize;
        for entry in dense_hits {
            let Some(src_bottom) = entry.hit.dest_y.checked_add(entry.height) else {
                return Err(WsiError::DisplayConversion(
                    "tile destination y overflow".into(),
                ));
            };
            if dst_y_i64 < entry.hit.dest_y || dst_y_i64 >= src_bottom {
                continue;
            }

            let dst_start_i64 = entry.hit.dest_x.max(0);
            let dst_end_i64 = entry
                .hit
                .dest_x
                .checked_add(entry.width)
                .ok_or_else(|| WsiError::DisplayConversion("tile destination x overflow".into()))?
                .min(out_width_i64);
            if dst_end_i64 <= dst_start_i64 {
                continue;
            }
            // Both coordinates are nonnegative and capped by a width that
            // originated as usize, so these conversions are lossless.
            let dst_start = dst_start_i64 as usize;
            let dst_end = dst_end_i64 as usize;
            if dst_start != cursor {
                return Ok(None);
            }

            // The row intersection checks above prove these source coordinates
            // are nonnegative and within the u32-sized decoded tile.
            let src_y = (dst_y_i64 - entry.hit.dest_y) as usize;
            let src_x = 0i64.max(-entry.hit.dest_x) as usize;
            let src_x_samples = src_x.checked_mul(shape.channels).ok_or_else(|| {
                WsiError::DisplayConversion("tile source x byte offset overflow".into())
            })?;
            let src_start = src_y
                .checked_mul(entry.row_stride)
                .and_then(|row| row.checked_add(src_x_samples))
                .ok_or_else(|| WsiError::DisplayConversion("tile source offset overflow".into()))?;
            let len = dst_end
                .checked_sub(dst_start)
                .and_then(|pixels| pixels.checked_mul(shape.channels))
                .ok_or_else(|| WsiError::DisplayConversion("tile copy length overflow".into()))?;
            let src_end = src_start
                .checked_add(len)
                .ok_or_else(|| WsiError::DisplayConversion("tile source end overflow".into()))?;
            let row = entry.data.get(src_start..src_end).ok_or_else(|| {
                WsiError::DisplayConversion("tile source row exceeds decoded buffer".into())
            })?;
            out.extend_from_slice(row);
            cursor = dst_end;
        }
        if cursor != shape.width {
            return Ok(None);
        }
    }

    if out.len() != total_samples {
        return Err(WsiError::DisplayConversion(format!(
            "dense compositor produced {} samples, expected {}",
            out.len(),
            total_samples
        )));
    }

    Ok(Some(out))
}
