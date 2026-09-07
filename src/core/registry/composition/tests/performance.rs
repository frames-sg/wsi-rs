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
