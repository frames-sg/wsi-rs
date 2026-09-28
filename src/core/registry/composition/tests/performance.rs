//! Differential oracles and repeatable composition-only experiments.
use super::region::CompositionShape;
use super::{fractional_u8, integral};
use crate::core::types::{ColorSpace, CpuTile, TileHit};
use std::hint::black_box;
use std::time::Instant;

fn hit(x: f64, y: f64, pixman: bool) -> TileHit {
    TileHit {
        col: 0,
        row: 0,
        dest_x: x.floor() as i64,
        dest_y: y.floor() as i64,
        dest_x_f64: x,
        dest_y_f64: y,
        cairo_fixed_dest: pixman.then_some((x, y)),
        cairo_rgb24: false,
    }
}

fn make_tile(width: u32, height: u32, channels: u16) -> CpuTile {
    let pixels = (0..width as usize * height as usize * channels as usize)
        .map(|n| ((n * 37 + n / 7) % 256) as u8)
        .collect();
    CpuTile::from_u8_interleaved(
        width,
        height,
        channels,
        match channels {
            1 => ColorSpace::Grayscale,
            3 => ColorSpace::Rgb,
            4 => ColorSpace::Rgba,
            _ => unreachable!(),
        },
        pixels,
    )
    .unwrap()
}

#[test]
fn opaque_pixman_clips_use_narrow_interpolation() {
    let data = (0..8)
        .flat_map(|y| (0..8).flat_map(move |x| [230 + x + 2 * y; 3]))
        .collect();
    let tile = CpuTile::from_u8_interleaved(8, 8, 3, ColorSpace::Rgb, data).unwrap();
    let shape = CompositionShape {
        width: 4,
        height: 4,
        channels: 3,
    };
    // Numeric results from Cairo 1.18.4 / Pixman 0.46.4, RGB24 source,
    // ARGB32 destination, SATURATE. A fully covered clip uses 7-bit integer
    // interpolation. Touching the bottom source edge keeps float sampling,
    // even when that edge's out-of-bounds tap has zero weight.
    for (dest_y, rgb24, first) in [(-2.0, true, 236u8), (-2.0, false, 237), (-4.0, true, 241)] {
        let mut pixels = vec![0; 48];
        let mut alpha = vec![0.0; 16];
        let mut placement = hit(-2.25, dest_y, true);
        placement.cairo_rgb24 = rgb24;
        fractional_u8::blit_fractional_saturating_u8(
            &mut pixels,
            &mut alpha,
            tile.as_u8().unwrap(),
            &tile,
            &placement,
            shape,
        );
        let expected = (0..4)
            .flat_map(|y| (0..4).flat_map(move |x| [first + x + 2 * y; 3]))
            .collect::<Vec<_>>();
        assert_eq!(pixels, expected, "source destination y={dest_y}");
        assert_eq!(alpha, [1.0; 16]);
    }
    let mut pixels = [10, 20, 30].repeat(16);
    let mut alpha = vec![128.0 / 255.0; 16];
    pixels[3..6].copy_from_slice(&[2, 4, 6]);
    alpha[1] = 1.0;
    let mut placement = hit(-2.25, -2.0, true);
    placement.cairo_rgb24 = true;
    fractional_u8::blit_fractional_saturating_u8(
        &mut pixels,
        &mut alpha,
        tile.as_u8().unwrap(),
        &tile,
        &placement,
        shape,
    );
    // Cairo retains the first painter's color and fills only missing coverage.
    assert_eq!(&pixels[..6], &[128, 138, 148, 2, 4, 6]);
    assert_eq!(alpha, [1.0; 16]);
}

#[test]
fn precomputed_fractional_sampling_matches_reference_pixels_and_alpha() {
    for channels in [1, 3, 4] {
        let tile = make_tile(13, 11, channels);
        let shape = CompositionShape {
            width: 17,
            height: 16,
            channels: channels as usize,
        };
        for pixman in [false, true] {
            for origin in [
                (-4.999, -3.25),
                (-0.00390625, 0.5),
                (0.25, 0.75),
                (9.9, 10.125),
                // Integral placements take the exact saturating copy path.
                (0.0, 0.0),
                (-3.0, 2.0),
                (4.0, -5.0),
            ] {
                let mut actual = vec![0; shape.width * shape.height * shape.channels];
                let mut expected = actual.clone();
                let mut actual_alpha = vec![0.; shape.width * shape.height];
                let mut expected_alpha = actual_alpha.clone();
                for (x, y) in [origin, (origin.0 + 2.5, origin.1 - 1.75), origin] {
                    let hit = hit(x, y, pixman);
                    fractional_u8::blit_fractional_saturating_u8(
                        &mut actual,
                        &mut actual_alpha,
                        tile.as_u8().unwrap(),
                        &tile,
                        &hit,
                        shape,
                    );
                    fractional_u8::reference::blit_fractional_saturating_u8(
                        &mut expected,
                        &mut expected_alpha,
                        tile.as_u8().unwrap(),
                        &tile,
                        &hit,
                        shape,
                    );
                    assert_eq!(
                        actual, expected,
                        "channels={channels}, origin={origin:?}, pixman={pixman}"
                    );
                    assert_eq!(actual_alpha, expected_alpha);
                }
            }
        }
    }
}

#[test]
#[ignore = "explicit release benchmark; WSI_RS_COMPOSITION_OUTPUT selects the JSON capture"]
fn composition_performance() {
    let mut records = Vec::new();
    for size in [256, 512, 1024, 2048] {
        let shape = CompositionShape {
            width: size,
            height: size,
            channels: 3,
        };
        let samples = size * size * 3;
        let count = (16 * 1024 * 1024 / (size * size)).max(4);
        let tile = make_tile(256, 256, 3);
        for unaligned in [false, true] {
            let offset = if unaligned { -1 } else { 0 };
            let across = size / 256 + usize::from(unaligned);
            let hits = (0..across)
                .flat_map(|row| {
                    (0..across).map(move |col| {
                        hit(
                            (col * 256) as f64 + offset as f64,
                            (row * 256) as f64 + offset as f64,
                            false,
                        )
                    })
                })
                .collect::<Vec<_>>();
            let dense = hits
                .iter()
                .map(|hit| integral::DenseIntegralU8Hit {
                    hit,
                    data: tile.as_u8().unwrap(),
                    width: 256,
                    height: 256,
                    row_stride: 256 * 3,
                })
                .collect::<Vec<_>>();
            let baseline = || {
                integral::reference::compose_dense_integral_u8_rows(
                    black_box(&dense),
                    shape,
                    samples,
                )
                .unwrap()
                .unwrap()
            };
            let candidate = || {
                integral::compose_dense_integral_u8_rows(black_box(&dense), shape, samples)
                    .unwrap()
                    .unwrap()
            };
            assert_eq!(baseline(), candidate());
            for pair in 0..5 {
                for candidate_first in [pair % 2 == 1, pair % 2 == 0] {
                    let start = Instant::now();
                    for _ in 0..count {
                        black_box(if candidate_first {
                            candidate()
                        } else {
                            baseline()
                        });
                    }
                    records.push(serde_json::json!({"workload":"dense", "size":size, "unaligned":unaligned, "pair":pair, "candidate":candidate_first, "iterations":count, "elapsed_ns":start.elapsed().as_nanos()}));
                }
            }
        }
        let tile = make_tile(size as u32, size as u32, 3);
        for pixman in [false, true] {
            let hit = hit(-0.25, -0.75, pixman);
            let run = |candidate: bool| {
                let mut output = vec![0; samples];
                let mut alpha = vec![0.; size * size];
                if candidate {
                    fractional_u8::blit_fractional_saturating_u8(
                        &mut output,
                        &mut alpha,
                        black_box(tile.as_u8().unwrap()),
                        &tile,
                        &hit,
                        shape,
                    );
                } else {
                    fractional_u8::reference::blit_fractional_saturating_u8(
                        &mut output,
                        &mut alpha,
                        black_box(tile.as_u8().unwrap()),
                        &tile,
                        &hit,
                        shape,
                    );
                }
                (output, alpha)
            };
            assert_eq!(run(false), run(true));
            for pair in 0..5 {
                for candidate in [pair % 2 == 1, pair % 2 == 0] {
                    let start = Instant::now();
                    for _ in 0..count {
                        black_box(run(candidate));
                    }
                    records.push(serde_json::json!({"workload":"fractional", "size":size, "pixman":pixman, "pair":pair, "candidate":candidate, "iterations":count, "elapsed_ns":start.elapsed().as_nanos()}));
                }
            }
        }
    }
    let path = std::env::var_os("WSI_RS_COMPOSITION_OUTPUT").expect("capture path");
    std::fs::write(path, serde_json::to_vec_pretty(&records).unwrap()).unwrap();
}
