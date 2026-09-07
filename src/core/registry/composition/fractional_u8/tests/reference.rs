use super::*;
pub(in crate::core::registry::composition) fn blit_fractional_saturating_u8(
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

    for out_y in start_y..end_y {
        for out_x in start_x..end_x {
            let sample = bilinear_sample(out_x, out_y, raster_dest, pixman_float_sampling);
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

            let in_bounds = |x: i64, y: i64| x >= 0 && x < tile_width && y >= 0 && y < tile_height;
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
