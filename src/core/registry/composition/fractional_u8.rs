use crate::core::registry::composition::region::CompositionShape;
use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout, TileHit};
use crate::error::WsiError;

pub(super) fn blit_fractional_saturating_u8(
    out: &mut [u8],
    alpha: &mut [f32],
    tile_data: &[u8],
    tile: &CpuTile,
    hit: &TileHit,
    shape: CompositionShape,
) {
    // Select the sampling arithmetic once; the per-pixel loop then has no
    // mode branches for LLVM to keep live across channels.
    if hit.cairo_fixed_dest.is_some() {
        blit_fractional::<true, false>(out, alpha, tile_data, &[], tile, hit, shape);
    } else {
        blit_fractional::<false, false>(out, alpha, tile_data, &[], tile, hit, shape);
    }
}

/// Whether a tile carries straight alpha that Pixman-compatible composition
/// treats as coverage, like OpenSlide's premultiplied ARGB tile surfaces.
pub(super) fn is_alpha_source(tile: &CpuTile) -> bool {
    tile.channels == 4
        && tile.color_space == ColorSpace::Rgba
        && tile.layout == CpuTileLayout::Interleaved
        && matches!(tile.data, CpuTileData::U8(_))
}

/// SATURATE-composites a straight-alpha RGBA tile into RGB output with the
/// source alpha as coverage. Cairo samples the premultiplied surface, so the
/// color is premultiplied once here with exact unorm8 rounding.
pub(super) fn blit_alpha_source_saturating_u8(
    out: &mut [u8],
    alpha: &mut [f32],
    tile: &CpuTile,
    hit: &TileHit,
    shape: CompositionShape,
) -> Result<(), WsiError> {
    let Some(rgba) = tile.data.as_u8().filter(|_| is_alpha_source(tile)) else {
        return Err(WsiError::DisplayConversion(
            "alpha-source composition expects interleaved RGBA8 tiles".into(),
        ));
    };
    if shape.channels != 3 || hit.cairo_fixed_dest.is_none() {
        return Err(WsiError::DisplayConversion(
            "alpha-source composition requires Pixman-placed RGB output".into(),
        ));
    }
    let pixels = rgba.len() / 4;
    let mut color = Vec::with_capacity(pixels * 3);
    let mut coverage = Vec::with_capacity(pixels);
    for pixel in rgba.chunks_exact(4) {
        let a = u16::from(pixel[3]);
        color.extend(
            pixel[..3]
                .iter()
                .map(|&c| ((u16::from(c) * a + 127) / 255) as u8),
        );
        coverage.push(pixel[3]);
    }
    blit_fractional::<true, true>(out, alpha, &color, &coverage, tile, hit, shape);
    Ok(())
}

fn blit_fractional<const PIXMAN: bool, const ALPHA: bool>(
    out: &mut [u8],
    alpha: &mut [f32],
    tile_data: &[u8],
    tile_alpha: &[u8],
    tile: &CpuTile,
    hit: &TileHit,
    shape: CompositionShape,
) {
    let tile_width = i64::from(tile.width);
    let tile_height = i64::from(tile.height);
    let raster_dest = hit
        .cairo_fixed_dest
        .unwrap_or((hit.dest_x_f64, hit.dest_y_f64));
    // The integral shortcut assumes an opaque source; alpha sources take the
    // general path, whose integral-position weights are exactly (1, 0).
    if PIXMAN && !ALPHA && raster_dest.0.fract() == 0.0 && raster_dest.1.fract() == 0.0 {
        let dest = (raster_dest.0 as i64, raster_dest.1 as i64);
        blit_integral_saturating(out, alpha, tile_data, tile, dest, shape);
        return;
    }
    let start_x = raster_dest.0.floor().max(0.0) as usize;
    let start_y = raster_dest.1.floor().max(0.0) as usize;
    let end_x = (raster_dest.0 + tile_width as f64)
        .ceil()
        .min(shape.width as f64) as usize;
    let end_y = (raster_dest.1 + tile_height as f64)
        .ceil()
        .min(shape.height as f64) as usize;
    let channels = shape.channels;
    let out_row_stride = shape.width * channels;
    let tile_row_stride = tile_width as usize * channels;

    // Admission reserves RGBA output. RGB/gray composition leaves enough space
    // for a bounded horizontal table; RGBA and very thin strips use scalar
    // sampling without an extra allocation.
    let table_bytes = (end_x - start_x).saturating_mul(std::mem::size_of::<(i64, f32, f32)>());
    let spare_output = shape
        .width
        .saturating_mul(shape.height)
        .saturating_mul(4_usize.saturating_sub(channels));
    let horizontal = (table_bytes <= spare_output).then(|| {
        (start_x..end_x)
            .map(|x| sampling_axis(x, raster_dest.0, PIXMAN))
            .collect::<Vec<_>>()
    });
    for out_y in start_y..end_y {
        let (y0, wy0, wy1) = sampling_axis(out_y, raster_dest.1, PIXMAN);
        let row0 = TileTap::axis(y0, tile_height, tile_row_stride, tile.width as usize);
        let row1 = TileTap::axis(y0 + 1, tile_height, tile_row_stride, tile.width as usize);
        let interior_row = PIXMAN && !ALPHA && y0 >= 0 && y0 + 1 < tile_height;
        let mut resume_x = start_x;
        for out_x in start_x..end_x {
            if out_x < resume_x {
                continue;
            }
            if let (true, Some(horizontal)) = (interior_row, horizontal.as_deref()) {
                let alpha_row = out_y * shape.width;
                let run = find_interior_run(
                    horizontal,
                    &alpha[alpha_row + start_x..alpha_row + end_x],
                    out_x - start_x,
                    tile_width,
                );
                if let Some((x0, wx0, wx1, pixels)) = run {
                    let row = out_y * out_row_stride + out_x * channels;
                    let source = x0 as usize * channels;
                    let row0 = y0 as usize * tile_row_stride + source;
                    let row1 = row0 + tile_row_stride;
                    let bytes = pixels * channels;
                    blit_opaque_interior_run(
                        &mut out[row..row + bytes],
                        &mut alpha[out_y * shape.width + out_x..][..pixels],
                        [
                            &tile_data[row0..row0 + bytes + channels],
                            &tile_data[row1..row1 + bytes + channels],
                        ],
                        [wx0 * wy0, wx1 * wy0, wx0 * wy1, wx1 * wy1],
                        channels,
                    );
                    resume_x = out_x + pixels;
                    continue;
                }
            }
            let (x0, wx0, wx1) = horizontal.as_ref().map_or_else(
                || sampling_axis(out_x, raster_dest.0, PIXMAN),
                |horizontal| horizontal[out_x - start_x],
            );
            let dest_offset = out_y * out_row_stride + out_x * channels;
            let alpha_offset = out_y * shape.width + out_x;
            let dest_alpha = alpha[alpha_offset];
            // A saturated destination takes nothing further from any source.
            if dest_alpha >= 1.0 {
                continue;
            }
            let col0 = TileTap::axis(x0, tile_width, channels, 1);
            let col1 = TileTap::axis(x0 + 1, tile_width, channels, 1);
            let taps = [
                row0.join(col0),
                row0.join(col1),
                row1.join(col0),
                row1.join(col1),
            ];
            // Out-of-bounds taps contribute zero weight and a zero sample,
            // exactly as the reference's skipped taps do.
            let weights = [
                taps[0].weight(wx0 * wy0),
                taps[1].weight(wx1 * wy0),
                taps[2].weight(wx0 * wy1),
                taps[3].weight(wx1 * wy1),
            ];
            let source_alpha = if PIXMAN {
                pixman_bilinear_interpolate(
                    [
                        taps[0].coverage::<ALPHA>(tile_alpha),
                        taps[1].coverage::<ALPHA>(tile_alpha),
                        taps[2].coverage::<ALPHA>(tile_alpha),
                        taps[3].coverage::<ALPHA>(tile_alpha),
                    ],
                    weights,
                )
            } else {
                weights[0] + weights[1] + weights[2] + weights[3]
            };
            if source_alpha <= 0.0 {
                continue;
            }
            // OpenSlide paints irregular tilemaps with Cairo's SATURATE
            // operator: source coverage may fill only the destination's
            // remaining alpha instead of replacing pixels already painted by
            // an earlier tile. Regular/integral blits never enter this path.
            let source_factor = ((1.0 - dest_alpha) / source_alpha).min(1.0);
            let out_alpha = if PIXMAN {
                source_alpha.mul_add(source_factor, dest_alpha)
            } else {
                source_alpha * source_factor + dest_alpha
            }
            .min(1.0);

            for channel in 0..channels {
                let sample = |tap: TileTap| unorm8_to_float(tap.sample(tile_data, channel), PIXMAN);
                let samples = [
                    sample(taps[0]),
                    sample(taps[1]),
                    sample(taps[2]),
                    sample(taps[3]),
                ];
                let destination = out[dest_offset + channel];
                let value = if PIXMAN {
                    let source_premult = pixman_bilinear_interpolate(samples, weights);
                    source_premult.mul_add(source_factor, unorm8_to_float(destination, true))
                } else {
                    let source_premult = samples[0] * weights[0]
                        + samples[1] * weights[1]
                        + samples[2] * weights[2]
                        + samples[3] * weights[3];
                    let dest_premult = (destination as f32 / 255.0) * dest_alpha;
                    let out_premult = source_premult * source_factor + dest_premult;
                    if out_alpha > 0.0 {
                        out_premult / out_alpha
                    } else {
                        0.0
                    }
                };
                out[dest_offset + channel] = contract_pixman_unorm8(value);
            }
            alpha[alpha_offset] = if PIXMAN {
                unorm8_to_float(contract_pixman_unorm8(out_alpha), true)
            } else {
                out_alpha
            };
        }
    }
}

/// Finds the run starting at `index` of the blit's horizontal sampling table
/// whose pixels sample consecutive in-bounds source columns with the first
/// pixel's weights and still have zero coverage. `alpha_row` is aligned with
/// the table. Returns the first source column, the weights and the length.
#[inline(always)]
fn find_interior_run(
    horizontal: &[(i64, f32, f32)],
    alpha_row: &[f32],
    index: usize,
    tile_width: i64,
) -> Option<(i64, f32, f32, usize)> {
    let (x0, wx0, wx1) = horizontal[index];
    let pixels = horizontal[index..]
        .iter()
        .zip(&alpha_row[index..])
        .zip(x0..)
        .take_while(|((&(x, w0, w1), &coverage), expected)| {
            x == *expected
                && x >= 0
                && x + 1 < tile_width
                && (w0, w1) == (wx0, wx1)
                && coverage == 0.0
        })
        .count();
    (pixels > 0).then_some((x0, wx0, wx1, pixels))
}

/// SATURATE for a run of zero-coverage pixels whose four taps are in bounds
/// and share one set of weights. This is the generic Pixman arithmetic with
/// the coverage terms constant, so LLVM can vectorize it across the run.
#[inline(always)]
fn blit_opaque_interior_run(
    out: &mut [u8],
    alpha: &mut [f32],
    rows: [&[u8]; 2],
    weights: [f32; 4],
    channels: usize,
) {
    let source_alpha = pixman_bilinear_interpolate([1.0; 4], weights);
    let source_factor = (1.0 / source_alpha).min(1.0);
    let out_alpha = source_alpha.mul_add(source_factor, 0.0).min(1.0);
    for (index, destination) in out.iter_mut().enumerate() {
        let samples = [
            rows[0][index],
            rows[0][index + channels],
            rows[1][index],
            rows[1][index + channels],
        ]
        .map(|sample| unorm8_to_float(sample, true));
        let source_premult = pixman_bilinear_interpolate(samples, weights);
        *destination = contract_pixman_unorm8(
            source_premult.mul_add(source_factor, unorm8_to_float(*destination, true)),
        );
    }
    alpha.fill(unorm8_to_float(contract_pixman_unorm8(out_alpha), true));
}

/// Pixman-exact SATURATE for an integral placement. Every output pixel samples
/// one source pixel with unit weight, so the bilinear arithmetic reduces to
/// this per-pixel form: covered pixels keep their value, uncovered pixels copy
/// the source exactly (`contract(s / 255) == s`), and partially covered pixels
/// blend with the remaining coverage.
fn blit_integral_saturating(
    out: &mut [u8],
    alpha: &mut [f32],
    tile_data: &[u8],
    tile: &CpuTile,
    dest: (i64, i64),
    shape: CompositionShape,
) {
    let channels = shape.channels;
    let x0 = dest.0.max(0);
    let y0 = dest.1.max(0);
    let x1 = (dest.0 + i64::from(tile.width)).min(shape.width as i64);
    let y1 = (dest.1 + i64::from(tile.height)).min(shape.height as i64);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let tile_row_stride = tile.width as usize * channels;
    let opaque = unorm8_to_float(u8::MAX, true);
    for out_y in y0..y1 {
        let source_row = (out_y - dest.1) as usize * tile_row_stride;
        let alpha_row = out_y as usize * shape.width;
        let row_alpha = &mut alpha[alpha_row + x0 as usize..alpha_row + x1 as usize];
        if row_alpha.iter().all(|&coverage| coverage == 0.0) {
            let source = source_row + (x0 - dest.0) as usize * channels;
            let target = (alpha_row + x0 as usize) * channels;
            let bytes = row_alpha.len() * channels;
            out[target..target + bytes].copy_from_slice(&tile_data[source..source + bytes]);
            row_alpha.fill(opaque);
            continue;
        }
        for out_x in x0..x1 {
            let pixel = alpha_row + out_x as usize;
            let dest_alpha = alpha[pixel];
            if dest_alpha >= 1.0 {
                continue;
            }
            let source = source_row + (out_x - dest.0) as usize * channels;
            let target = pixel * channels;
            if dest_alpha == 0.0 {
                // A pixel with zero coverage still holds zero samples.
                out[target..target + channels]
                    .copy_from_slice(&tile_data[source..source + channels]);
                alpha[pixel] = opaque;
                continue;
            }
            let factor = (1.0 - dest_alpha).min(1.0);
            let out_alpha = 1.0_f32.mul_add(factor, dest_alpha).min(1.0);
            for channel in 0..channels {
                let value = unorm8_to_float(tile_data[source + channel], true)
                    .mul_add(factor, unorm8_to_float(out[target + channel], true));
                out[target + channel] = contract_pixman_unorm8(value);
            }
            alpha[pixel] = unorm8_to_float(contract_pixman_unorm8(out_alpha), true);
        }
    }
}

/// One bilinear source tap. An out-of-bounds tap reads byte zero of the tile
/// and masks it, so the channel loop stays branch-free and in bounds.
#[derive(Clone, Copy)]
struct TileTap {
    offset: usize,
    pixel: usize,
    mask: u8,
}

impl TileTap {
    #[inline(always)]
    fn axis(position: i64, extent: i64, stride: usize, pixel_stride: usize) -> Self {
        if (0..extent).contains(&position) {
            Self {
                offset: position as usize * stride,
                pixel: position as usize * pixel_stride,
                mask: u8::MAX,
            }
        } else {
            Self {
                offset: 0,
                pixel: 0,
                mask: 0,
            }
        }
    }

    #[inline(always)]
    fn join(self, column: Self) -> Self {
        let mask = self.mask & column.mask;
        if mask == 0 {
            return Self {
                offset: 0,
                pixel: 0,
                mask,
            };
        }
        Self {
            offset: self.offset + column.offset,
            pixel: self.pixel + column.pixel,
            mask,
        }
    }

    #[inline(always)]
    fn weight(self, weight: f32) -> f32 {
        if self.mask == 0 {
            0.0
        } else {
            weight
        }
    }

    #[inline(always)]
    fn coverage<const ALPHA: bool>(self, tile_alpha: &[u8]) -> f32 {
        if ALPHA {
            if self.mask == 0 {
                0.0
            } else {
                unorm8_to_float(tile_alpha[self.pixel], true)
            }
        } else {
            f32::from(self.mask & 1)
        }
    }

    #[inline(always)]
    fn sample(self, tile_data: &[u8], channel: usize) -> u8 {
        tile_data[self.offset + channel] & self.mask
    }
}

fn sampling_axis(out: usize, dest: f64, pixman: bool) -> (i64, f32, f32) {
    let source = out as f64 - dest;
    let low = source.floor() as i64;
    let fraction = source - low as f64;
    let high_weight = fraction as f32;
    let low_weight = if pixman {
        1.0_f32 - high_weight
    } else {
        (1.0 - fraction) as f32
    };
    (low, low_weight, high_weight)
}

#[cfg(test)]
#[path = "fractional_u8/tests/reference.rs"]
pub(super) mod reference;

#[cfg(test)]
#[derive(Clone, Copy)]
struct BilinearSample {
    x0: i64,
    x1: i64,
    y0: i64,
    y1: i64,
    a00: f32,
    a10: f32,
    a01: f32,
    a11: f32,
}

#[cfg(test)]
fn bilinear_sample(
    out_x: usize,
    out_y: usize,
    dest: (f64, f64),
    pixman_float_sampling: bool,
) -> BilinearSample {
    let src_x = out_x as f64 - dest.0;
    let src_y = out_y as f64 - dest.1;
    let x0 = src_x.floor() as i64;
    let y0 = src_y.floor() as i64;
    let wx1 = (src_x - x0 as f64) as f32;
    let wy1 = (src_y - y0 as f64) as f32;
    let wx0 = if pixman_float_sampling {
        1.0_f32 - wx1
    } else {
        (1.0 - (src_x - x0 as f64)) as f32
    };
    let wy0 = if pixman_float_sampling {
        1.0_f32 - wy1
    } else {
        (1.0 - (src_y - y0 as f64)) as f32
    };
    BilinearSample {
        x0,
        x1: x0 + 1,
        y0,
        y1: y0 + 1,
        a00: wx0 * wy0,
        a10: wx1 * wy0,
        a01: wx0 * wy1,
        a11: wx1 * wy1,
    }
}

#[inline]
pub(super) fn pixman_bilinear_interpolate(values: [f32; 4], weights: [f32; 4]) -> f32 {
    values[3].mul_add(
        weights[3],
        values[2].mul_add(
            weights[2],
            values[1].mul_add(weights[1], values[0] * weights[0]),
        ),
    )
}

pub(super) fn unpremultiply_u8(pixels: &mut [u8], alpha: &[f32], channels: usize) {
    for (pixel, &alpha) in pixels.chunks_exact_mut(channels).zip(alpha) {
        let alpha = (alpha * 255.0).round() as u16;
        if alpha == 0 {
            pixel.fill(0);
        } else if alpha < 255 {
            for channel in pixel {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

/// Straight RGBA from premultiplied RGB and its composed coverage, with the
/// same per-pixel arithmetic as [`unpremultiply_u8`] and a Pixman alpha byte.
pub(super) fn unpremultiplied_rgba_u8(premultiplied: &[u8], alpha: &[f32]) -> Vec<u8> {
    let mut rgba = vec![0u8; alpha.len() * 4];
    if alpha.iter().all(|&coverage| coverage == 1.0) {
        for (target, source) in rgba.chunks_exact_mut(4).zip(premultiplied.chunks_exact(3)) {
            target.copy_from_slice(&[source[0], source[1], source[2], u8::MAX]);
        }
        return rgba;
    }
    for ((target, source), &coverage) in rgba
        .chunks_exact_mut(4)
        .zip(premultiplied.chunks_exact(3))
        .zip(alpha)
    {
        target[3] = contract_pixman_unorm8(coverage);
        match (coverage * 255.0).round() as u16 {
            0 => {}
            alpha @ 1..=254 => {
                for (target, &channel) in target[..3].iter_mut().zip(source) {
                    *target = ((u16::from(channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
            _ => target[..3].copy_from_slice(source),
        }
    }
    rgba
}

#[inline]
pub(super) fn unorm8_to_float(value: u8, pixman_float_sampling: bool) -> f32 {
    if pixman_float_sampling {
        value as f32 * (1.0_f32 / 255.0_f32)
    } else {
        value as f32 / 255.0_f32
    }
}

pub(super) fn contract_pixman_unorm8(value: f32) -> u8 {
    let quantized = (value.clamp(0.0, 1.0) * 256.0) as u16;
    (quantized - (quantized >> 8)) as u8
}
