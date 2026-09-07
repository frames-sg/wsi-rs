use crate::core::registry::composition::region::CompositionShape;
use crate::core::types::{CpuTile, TileHit};

pub(super) fn blit_fractional_saturating_u8(
    out: &mut [u8],
    alpha: &mut [f32],
    tile_data: &[u8],
    tile: &CpuTile,
    hit: &TileHit,
    shape: CompositionShape,
) {
    let tile_width = i64::from(tile.width);
    let tile_height = i64::from(tile.height);
    let pixman_float_sampling = hit.cairo_fixed_dest.is_some();
    let raster_dest = hit
        .cairo_fixed_dest
        .unwrap_or((hit.dest_x_f64, hit.dest_y_f64));
    let start_x = raster_dest.0.floor().max(0.0) as usize;
    let start_y = raster_dest.1.floor().max(0.0) as usize;
    let end_x = (raster_dest.0 + tile_width as f64)
        .ceil()
        .min(shape.width as f64) as usize;
    let end_y = (raster_dest.1 + tile_height as f64)
        .ceil()
        .min(shape.height as f64) as usize;
    let out_row_stride = shape.width * shape.channels;
    let tile_row_stride = tile_width as usize * shape.channels;

    // Admission reserves RGBA output. RGB/gray composition leaves enough space
    // for a bounded horizontal table; RGBA and very thin strips use scalar
    // sampling without an extra allocation.
    let table_bytes = (end_x - start_x).saturating_mul(std::mem::size_of::<(i64, f32, f32)>());
    let spare_output = shape
        .width
        .saturating_mul(shape.height)
        .saturating_mul(4_usize.saturating_sub(shape.channels));
    let horizontal = (table_bytes <= spare_output).then(|| {
        (start_x..end_x)
            .map(|x| sampling_axis(x, raster_dest.0, pixman_float_sampling))
            .collect::<Vec<_>>()
    });
    for out_y in start_y..end_y {
        let (y0, wy0, wy1) = sampling_axis(out_y, raster_dest.1, pixman_float_sampling);
        for out_x in start_x..end_x {
            let (x0, wx0, wx1) = horizontal.as_ref().map_or_else(
                || sampling_axis(out_x, raster_dest.0, pixman_float_sampling),
                |horizontal| horizontal[out_x - start_x],
            );
            let sample = BilinearSample {
                x0,
                x1: x0 + 1,
                y0,
                y1: y0 + 1,
                a00: wx0 * wy0,
                a10: wx1 * wy0,
                a01: wx0 * wy1,
                a11: wx1 * wy1,
            };
            let BilinearSample {
                x0,
                x1,
                y0,
                y1,
                a00,
                a10,
                a01,
                a11,
            } = sample;
            let dest_offset = out_y * out_row_stride + out_x * shape.channels;
            let alpha_offset = out_y * shape.width + out_x;

            let interior = x0 >= 0 && x1 < tile_width && y0 >= 0 && y1 < tile_height;
            let in_bounds = |x: i64, y: i64| {
                interior || (x >= 0 && x < tile_width && y >= 0 && y < tile_height)
            };
            let a00 = if in_bounds(x0, y0) { a00 } else { 0.0 };
            let a10 = if in_bounds(x1, y0) { a10 } else { 0.0 };
            let a01 = if in_bounds(x0, y1) { a01 } else { 0.0 };
            let a11 = if in_bounds(x1, y1) { a11 } else { 0.0 };
            let source_alpha = if pixman_float_sampling {
                pixman_bilinear_interpolate(
                    [
                        in_bounds(x0, y0) as u8 as f32,
                        in_bounds(x1, y0) as u8 as f32,
                        in_bounds(x0, y1) as u8 as f32,
                        in_bounds(x1, y1) as u8 as f32,
                    ],
                    [a00, a10, a01, a11],
                )
            } else {
                a00 + a10 + a01 + a11
            };
            if source_alpha <= 0.0 {
                continue;
            }

            let p00 = in_bounds(x0, y0)
                .then(|| (y0 as usize * tile_row_stride) + x0 as usize * shape.channels);
            let p10 = in_bounds(x1, y0)
                .then(|| (y0 as usize * tile_row_stride) + x1 as usize * shape.channels);
            let p01 = in_bounds(x0, y1)
                .then(|| (y1 as usize * tile_row_stride) + x0 as usize * shape.channels);
            let p11 = in_bounds(x1, y1)
                .then(|| (y1 as usize * tile_row_stride) + x1 as usize * shape.channels);
            let dest_alpha = alpha[alpha_offset];
            if dest_alpha >= 1.0 {
                continue;
            }
            // OpenSlide paints irregular tilemaps with Cairo's SATURATE
            // operator: source coverage may fill only the destination's
            // remaining alpha instead of replacing pixels already painted by
            // an earlier tile. Regular/integral blits never enter this path.
            let source_factor = ((1.0 - dest_alpha) / source_alpha).min(1.0);
            let out_alpha = if pixman_float_sampling {
                source_alpha.mul_add(source_factor, dest_alpha)
            } else {
                source_alpha * source_factor + dest_alpha
            }
            .min(1.0);

            for channel in 0..shape.channels {
                let source_premult = if pixman_float_sampling {
                    let samples = [p00, p10, p01, p11];
                    pixman_bilinear_interpolate(
                        samples.map(|index| {
                            index
                                .map(|index| unorm8_to_float(tile_data[index + channel], true))
                                .unwrap_or(0.0)
                        }),
                        [a00, a10, a01, a11],
                    )
                } else {
                    p00.map(|index| unorm8_to_float(tile_data[index + channel], false) * a00)
                        .unwrap_or(0.0)
                        + p10
                            .map(|index| unorm8_to_float(tile_data[index + channel], false) * a10)
                            .unwrap_or(0.0)
                        + p01
                            .map(|index| unorm8_to_float(tile_data[index + channel], false) * a01)
                            .unwrap_or(0.0)
                        + p11
                            .map(|index| unorm8_to_float(tile_data[index + channel], false) * a11)
                            .unwrap_or(0.0)
                };
                let dest_premult = if pixman_float_sampling {
                    unorm8_to_float(out[dest_offset + channel], true)
                } else {
                    (out[dest_offset + channel] as f32 / 255.0) * dest_alpha
                };
                let out_premult = if pixman_float_sampling {
                    source_premult.mul_add(source_factor, dest_premult)
                } else {
                    source_premult * source_factor + dest_premult
                };
                let value = if pixman_float_sampling {
                    out_premult
                } else if out_alpha > 0.0 {
                    out_premult / out_alpha
                } else {
                    0.0
                };
                out[dest_offset + channel] = contract_pixman_unorm8(value);
            }
            alpha[alpha_offset] = if pixman_float_sampling {
                unorm8_to_float(contract_pixman_unorm8(out_alpha), true)
            } else {
                out_alpha
            };
        }
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
