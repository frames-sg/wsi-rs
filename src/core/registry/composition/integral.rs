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
    let Some(clip) = IntegralClip::of_hit(tile, hit, shape) else {
        return;
    };
    let tile_row_stride = tile.width as usize * shape.channels;
    let out_row_stride = shape.width * shape.channels;

    for row in 0..clip.height() {
        let src_offset = (clip.source_y + row) * tile_row_stride + clip.source_x * shape.channels;
        let dest_offset = (clip.y0 + row) * out_row_stride + clip.x0 * shape.channels;
        let len = clip.width() * shape.channels;
        out[dest_offset..dest_offset + len]
            .copy_from_slice(&tile_data[src_offset..src_offset + len]);
    }
}

/// A tile placed at integral output coordinates, clipped to the output: it
/// covers output pixels `x0..x1` by `y0..y1`, starting from tile pixel
/// (`source_x`, `source_y`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct IntegralClip {
    pub(super) x0: usize,
    pub(super) y0: usize,
    pub(super) x1: usize,
    pub(super) y1: usize,
    pub(super) source_x: usize,
    pub(super) source_y: usize,
}

impl IntegralClip {
    /// Clips a `size` tile at `dest` to an `output` image, or `None` when
    /// they do not overlap. A placement beyond the `i64` range clips like any
    /// other off-output placement.
    pub(super) fn new(dest: (i64, i64), size: (u32, u32), output: (usize, usize)) -> Option<Self> {
        let x0 = dest.0.max(0);
        let y0 = dest.1.max(0);
        let x1 = dest
            .0
            .saturating_add(i64::from(size.0))
            .min(i64::try_from(output.0).unwrap_or(i64::MAX));
        let y1 = dest
            .1
            .saturating_add(i64::from(size.1))
            .min(i64::try_from(output.1).unwrap_or(i64::MAX));
        // An overlap puts `dest` within one tile size of the output, so the
        // source offsets fit in `u32`.
        (x0 < x1 && y0 < y1).then(|| Self {
            x0: x0 as usize,
            y0: y0 as usize,
            x1: x1 as usize,
            y1: y1 as usize,
            source_x: (x0 - dest.0) as usize,
            source_y: (y0 - dest.1) as usize,
        })
    }

    pub(super) fn of_hit(tile: &CpuTile, hit: &TileHit, shape: CompositionShape) -> Option<Self> {
        Self::new(
            (hit.dest_x, hit.dest_y),
            (tile.width, tile.height),
            (shape.width, shape.height),
        )
    }

    pub(super) fn width(&self) -> usize {
        self.x1 - self.x0
    }

    pub(super) fn height(&self) -> usize {
        self.y1 - self.y0
    }
}

pub(super) fn blit_integral_rgb_into_rgba(
    out: &mut [u8],
    source: &[u8],
    tile: &CpuTile,
    hit: &TileHit,
    shape: CompositionShape,
) {
    let Some(clip) = IntegralClip::of_hit(tile, hit, shape) else {
        return;
    };
    for row in 0..clip.height() {
        let start = ((clip.source_y + row) * tile.width as usize + clip.source_x) * 3;
        let target = ((clip.y0 + row) * shape.width + clip.x0) * 4;
        let source = source[start..start + clip.width() * 3].as_chunks::<3>().0;
        let output = out[target..target + clip.width() * 4]
            .as_chunks_mut::<4>()
            .0;
        for (pixel, &[r, g, b]) in output.iter_mut().zip(source) {
            *pixel = [r, g, b, 255];
        }
    }
}

pub(super) fn mark_integral_tile_opaque(
    alpha: &mut [f32],
    tile: &CpuTile,
    hit: &TileHit,
    shape: CompositionShape,
) {
    let Some(clip) = IntegralClip::of_hit(tile, hit, shape) else {
        return;
    };
    for y in clip.y0..clip.y1 {
        let dest_offset = y * shape.width + clip.x0;
        alpha[dest_offset..dest_offset + clip.width()].fill(1.0);
    }
}

pub(super) fn is_integral_hit(hit: &TileHit) -> bool {
    hit.cairo_fixed_dest.is_none()
        && (hit.dest_x_f64 - hit.dest_x as f64).abs() <= 1e-6
        && (hit.dest_y_f64 - hit.dest_y as f64).abs() <= 1e-6
}

/// Whether the hit lands on whole pixels, including Pixman-placed hits whose
/// raster position is integral. Copying such a hit is exact when nothing else
/// covers its pixels.
pub(super) fn has_integral_position(hit: &TileHit) -> bool {
    is_integral_hit(hit)
        || hit.cairo_fixed_dest == Some((hit.dest_x as f64, hit.dest_y as f64))
            && (hit.dest_x_f64, hit.dest_y_f64) == (hit.dest_x as f64, hit.dest_y as f64)
}

pub(super) fn hit_covers_output(hit: &TileHit, tile: &CpuTile, width: u32, height: u32) -> bool {
    has_integral_position(hit)
        && hit.dest_x == 0
        && hit.dest_y == 0
        && tile.width == width
        && tile.height == height
}

pub(super) struct DenseIntegralU8Hit<'a> {
    pub(super) hit: &'a TileHit,
    pub(super) data: &'a [u8],
    pub(super) width: u32,
    pub(super) height: u32,
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
        // Dense rows reject overlaps and gaps, so integral placements copy exactly.
        if !has_integral_position(hit)
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
            width: tile.width,
            height: tile.height,
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
    let Some(spans) = dense_row_spans(dense_hits, shape)? else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(total_samples);
    for y in 0..shape.height {
        for span in spans.iter().filter(|span| span.covers_row(y)) {
            out.extend_from_slice(span.source_row(y)?);
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

/// One integral tile's clipped rectangle in a dense composition.
struct DenseSpan<'a> {
    data: &'a [u8],
    top: usize,
    bottom: usize,
    /// Tile row drawn at output row `top`.
    source_y: usize,
    start: usize,
    end: usize,
    /// Byte offset of the first drawn sample in each tile row.
    source_x: usize,
    row_stride: usize,
    len: usize,
}

impl<'a> DenseSpan<'a> {
    fn covers_row(&self, y: usize) -> bool {
        (self.top..self.bottom).contains(&y)
    }

    /// Source samples this span contributes to output row `y`.
    fn source_row(&self, y: usize) -> Result<&'a [u8], WsiError> {
        // A covered row lies within the u32-sized tile, so the row index fits.
        let start = (self.source_y + (y - self.top))
            .checked_mul(self.row_stride)
            .and_then(|offset| offset.checked_add(self.source_x))
            .ok_or_else(|| WsiError::DisplayConversion("tile source offset overflow".into()))?;
        let end = start
            .checked_add(self.len)
            .ok_or_else(|| WsiError::DisplayConversion("tile source end overflow".into()))?;
        self.data.get(start..end).ok_or_else(|| {
            WsiError::DisplayConversion("tile source row exceeds decoded buffer".into())
        })
    }
}

/// Clipped spans that cover every output row exactly once, left to right in
/// `dense_hits` order, or `None` when rows have gaps or overlaps.
fn dense_row_spans<'a>(
    dense_hits: &[DenseIntegralU8Hit<'a>],
    shape: CompositionShape,
) -> Result<Option<Vec<DenseSpan<'a>>>, WsiError> {
    let mut spans = Vec::with_capacity(dense_hits.len());
    for entry in dense_hits {
        let Some(clip) = IntegralClip::new(
            (entry.hit.dest_x, entry.hit.dest_y),
            (entry.width, entry.height),
            (shape.width, shape.height),
        ) else {
            continue;
        };
        let source_x = clip.source_x.checked_mul(shape.channels).ok_or_else(|| {
            WsiError::DisplayConversion("tile source x byte offset overflow".into())
        })?;
        let len = clip
            .width()
            .checked_mul(shape.channels)
            .ok_or_else(|| WsiError::DisplayConversion("tile copy length overflow".into()))?;
        spans.push(DenseSpan {
            data: entry.data,
            top: clip.y0,
            bottom: clip.y1,
            source_y: clip.source_y,
            start: clip.x0,
            end: clip.x1,
            source_x,
            row_stride: entry.row_stride,
            len,
        });
    }
    for y in 0..shape.height {
        let mut cursor = 0;
        for span in spans.iter().filter(|span| span.covers_row(y)) {
            if span.start != cursor {
                return Ok(None);
            }
            cursor = span.end;
        }
        if cursor != shape.width {
            return Ok(None);
        }
    }
    Ok(Some(spans))
}

/// Writes cached opaque RGB tiles that densely cover a region straight into
/// premultiplied ARGB32 pixels, the layout OpenSlide and Cairo use. Returns
/// `false` without writing when the tiles are not integral, dense, 8-bit RGB.
pub(super) fn compose_dense_integral_rgb_argb32(
    hits: &[TileHit],
    hit_tiles: &[Arc<CpuTile>],
    width: u32,
    height: u32,
    destination: &mut [u32],
) -> Result<bool, WsiError> {
    if hits.len() != hit_tiles.len() || hits.is_empty() {
        return Ok(false);
    }
    let shape = CompositionShape {
        width: width as usize,
        height: height as usize,
        channels: 3,
    };
    if shape.width.checked_mul(shape.height) != Some(destination.len()) {
        return Err(WsiError::DisplayConversion(format!(
            "ARGB destination has {} pixels, expected {width}x{height}",
            destination.len()
        )));
    }
    let Some(dense_hits) = collect_dense_integral_u8_hits(
        hits,
        hit_tiles,
        3,
        &ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        3,
    )?
    else {
        return Ok(false);
    };
    let Some(spans) = dense_row_spans(&dense_hits, shape)? else {
        return Ok(false);
    };
    for (y, row) in destination.chunks_exact_mut(shape.width).enumerate() {
        for span in spans.iter().filter(|span| span.covers_row(y)) {
            rgb_to_opaque_argb32(span.source_row(y)?, &mut row[span.start..span.end]);
        }
    }
    Ok(true)
}

/// SATURATE-paints an opaque interleaved RGB8 tile at integral position `dest`
/// into premultiplied RGBA output of size `output`. Integral opaque SATURATE
/// needs only one RGBA image: covered pixels keep the first painter's color.
pub(super) fn blit_integral_rgb_saturating_rgba(
    out: &mut [u8],
    output: (u32, u32),
    source: &[u8],
    tile: &CpuTile,
    dest: (i64, i64),
) {
    let Some(clip) = IntegralClip::new(
        dest,
        (tile.width, tile.height),
        (output.0 as usize, output.1 as usize),
    ) else {
        return;
    };
    let count = clip.width();
    for row in 0..clip.height() {
        let target = ((clip.y0 + row) * output.0 as usize + clip.x0) * 4;
        let start = ((clip.source_y + row) * tile.width as usize + clip.source_x) * 3;
        let row = &mut out[target..target + count * 4];
        let source = source[start..start + count * 3].as_chunks::<3>().0;
        if row.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0) {
            // Most tile rows have no overlap. Keep the RGB-to-RGBA loop free
            // of per-pixel branches.
            for (target, &[r, g, b]) in row.as_chunks_mut::<4>().0.iter_mut().zip(source) {
                *target = [r, g, b, 255];
            }
        } else {
            for (target, &[r, g, b]) in row.as_chunks_mut::<4>().0.iter_mut().zip(source) {
                if target[3] == 0 {
                    *target = [r, g, b, 255];
                }
            }
        }
    }
}

fn rgb_to_opaque_argb32(rgb: &[u8], argb: &mut [u32]) {
    // Whole-pixel arrays carry no bounds checks, so the loop vectorizes.
    let (pixels, _) = rgb.as_chunks::<3>();
    for (dest, &[r, g, b]) in argb.iter_mut().zip(pixels) {
        *dest = 0xff00_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
    }
}

#[cfg(test)]
#[path = "integral/tests/reference.rs"]
pub(super) mod reference;
