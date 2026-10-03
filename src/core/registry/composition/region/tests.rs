use super::super::fractional_u8::{pixman_bilinear_interpolate, unorm8_to_float};
use super::super::integral::{compose_dense_integral_u8_rows, DenseIntegralU8Hit};
use super::super::output::crop_rgb_interleaved_u8_buffer;
use super::*;
use crate::core::types::{Dataset, DatasetId, TileEntry, TileLayout, TileRequest};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) fn composite_fractional_region_from_source<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    origin_px: (f64, f64),
    max_region_pixels: u64,
) -> Result<CpuTile, WsiError> {
    let plan = RegionReadPlan::fractional(source.dataset(), req, origin_px, max_region_pixels)?;
    compose_resolved_region(source, cache, req, plan)
}

pub(super) fn composite_region_from_source_streaming<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    max_region_pixels: u64,
) -> Result<CpuTile, WsiError> {
    let plan = RegionReadPlan::integral(source.dataset(), req, max_region_pixels)?;
    compose_resolved_region_streaming(source, cache, req, plan, 1)
}

struct StreamingSource {
    dataset: Dataset,
    single_reads: AtomicUsize,
    batch_reads: AtomicUsize,
}

impl SlideReader for StreamingSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.single_reads.fetch_add(1, Ordering::SeqCst);
        let value = u8::try_from(req.col + 1).unwrap_or(0);
        CpuTile::from_u8_interleaved(2, 1, 3, ColorSpace::Rgb, vec![value; 2 * 3])
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.batch_reads.fetch_add(1, Ordering::SeqCst);
        reqs.iter().map(|req| self.read_tile_cpu(req)).collect()
    }
}

fn hit(dest_x: i64, dest_x_f64: f64) -> TileHit {
    hit_at(dest_x, 0, dest_x_f64, 0.0)
}

fn hit_at(dest_x: i64, dest_y: i64, dest_x_f64: f64, dest_y_f64: f64) -> TileHit {
    TileHit {
        col: dest_x,
        row: dest_y,
        dest_x,
        dest_y,
        dest_x_f64,
        dest_y_f64,
        cairo_fixed_dest: None,
        cairo_rgb24: false,
    }
}

fn tile(data: CpuTileData, width: u32) -> Arc<CpuTile> {
    Arc::new(CpuTile {
        width,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data,
    })
}

fn tile_2d(data: CpuTileData, width: u32, height: u32) -> Arc<CpuTile> {
    Arc::new(CpuTile {
        width,
        height,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data,
    })
}

#[test]
fn streaming_region_resolves_one_source_tile_at_a_time() {
    let source = StreamingSource {
        dataset: crate::test_support::regular_rgb_dataset_for_test(
            DatasetId::new(77),
            "s0",
            "ser0",
            crate::test_support::RegularLevelForTest {
                dimensions: (4, 1),
                tile_width: 2,
                tile_height: 1,
                tiles_across: 2,
                tiles_down: 1,
            },
        ),
        single_reads: AtomicUsize::new(0),
        batch_reads: AtomicUsize::new(0),
    };
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (4, 1));

    let region =
        composite_region_from_source_streaming(&source, None, &req, 16).expect("streamed region");

    assert_eq!(
        region.as_u8().unwrap(),
        &[1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2]
    );
    assert_eq!(source.single_reads.load(Ordering::SeqCst), 2);
    assert_eq!(source.batch_reads.load(Ordering::SeqCst), 0);
}

#[test]
fn fractional_u8_composition_saturates_prior_integral_tile() {
    let hits = [hit(0, 0.0), hit(2, 1.5)];
    let tiles = [
        tile(CpuTileData::u8(vec![0, 0]), 2),
        tile(CpuTileData::u8(vec![255, 255]), 2),
    ];

    let composed = compose_region_tiles(&hits, &tiles, 4, 1, false).unwrap();

    assert_eq!(composed.data.as_u8().unwrap(), &[0, 0, 255, 255]);
}

#[test]
fn integral_rgba_composition_preserves_gaps_overlap_and_later_alpha_sources() {
    let hits: Vec<_> = [-1, 1, 2]
        .into_iter()
        .map(|x| {
            let mut hit = hit(x, x as f64);
            hit.cairo_fixed_dest = Some((x as f64, 0.0));
            hit
        })
        .collect();
    let first =
        CpuTile::from_u8_interleaved(3, 2, 3, ColorSpace::Rgb, [10, 20, 30].repeat(6)).unwrap();
    let second =
        CpuTile::from_u8_interleaved(2, 2, 3, ColorSpace::Rgb, [40, 50, 60].repeat(4)).unwrap();
    for last in [
        CpuTile::from_u8_interleaved(2, 2, 3, ColorSpace::Rgb, [200, 100, 50].repeat(4)).unwrap(),
        CpuTile::from_u8_interleaved(2, 2, 4, ColorSpace::Rgba, [200, 100, 50, 128].repeat(4))
            .unwrap(),
    ] {
        let mut composer = RegionComposer::new(5, 2, &first, true, &hits).unwrap();
        let mut reference = vec![0; 5 * 2 * 3];
        let mut coverage = vec![0.0; 5 * 2];
        let shape = CompositionShape {
            width: 5,
            height: 2,
            channels: 3,
        };
        for (hit, tile) in hits.iter().zip([&first, &second, &last]) {
            composer.blit(hit, tile).unwrap();
            if is_alpha_source(tile) {
                blit_alpha_source_saturating_u8(&mut reference, &mut coverage, tile, hit, shape)
                    .unwrap();
            } else {
                blit_fractional_saturating_u8(
                    &mut reference,
                    &mut coverage,
                    tile.as_u8().unwrap(),
                    tile,
                    hit,
                    shape,
                );
            }
        }
        let actual = composer.finish().unwrap().into_rgba().unwrap();
        assert_eq!(
            actual.as_raw(),
            &unpremultiplied_rgba_u8(&reference, &coverage)
        );
        assert_eq!(actual.get_pixel(0, 0).0, [10, 20, 30, 255]);
        assert_eq!(actual.get_pixel(1, 0).0, [10, 20, 30, 255]);
        assert_eq!(actual.get_pixel(2, 0).0, [40, 50, 60, 255]);
        assert_eq!(actual.get_pixel(4, 0).0, [0, 0, 0, 0]);
    }
}

#[test]
fn direct_rgba_bands_match_complete_float_composition() {
    use crate::core::registry::composition::fractional_u8::{
        blit_premultiplied_rgba_in_bands, unpremultiply_rgba, RgbaBandScratch,
    };

    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move |bound: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % bound
    };
    let (width, height) = (41usize, 57usize);
    let shape = CompositionShape {
        width,
        height,
        channels: 3,
    };
    for _ in 0..48 {
        let tiles: Vec<(TileHit, CpuTile)> = (0..6)
            .map(|_| {
                // Small tiles, and large ones whose opaque clips select
                // Pixman's narrow filter.
                let (tile_width, tile_height) = if next(2) == 0 {
                    (1 + next(33) as u32, 1 + next(47) as u32)
                } else {
                    (48 + next(24) as u32, 64 + next(24) as u32)
                };
                let pixels = (tile_width * tile_height) as usize;
                let tile = if next(3) == 0 {
                    let alpha = [0, 255, 1 + next(254) as u8][next(3) as usize];
                    let data = (0..pixels * 4)
                        .map(|i| if i % 4 == 3 { alpha } else { next(256) as u8 })
                        .collect();
                    CpuTile::from_u8_interleaved(tile_width, tile_height, 4, ColorSpace::Rgba, data)
                } else {
                    let data = (0..pixels * 3).map(|_| next(256) as u8).collect();
                    CpuTile::from_u8_interleaved(tile_width, tile_height, 3, ColorSpace::Rgb, data)
                }
                .unwrap();
                // Whole and fractional placements, including off-canvas origins.
                let position = |extent: usize, next: &mut dyn FnMut(u64) -> u64| {
                    let whole = next(extent as u64 + 24) as f64 - 20.0;
                    whole + [0.0, next(64) as f64 / 64.0][next(2) as usize]
                };
                let (x, y) = (position(width, &mut next), position(height, &mut next));
                let mut hit = hit_at(x.floor() as i64, y.floor() as i64, x, y);
                hit.cairo_fixed_dest = Some((x, y));
                hit.cairo_rgb24 = next(2) == 0;
                (hit, tile)
            })
            .collect();

        let mut reference = vec![0; width * height * 3];
        let mut coverage = vec![0.0; width * height];
        for (hit, tile) in &tiles {
            if is_alpha_source(tile) {
                blit_alpha_source_saturating_u8(&mut reference, &mut coverage, tile, hit, shape)
                    .unwrap();
            } else {
                let data = tile.as_u8().unwrap();
                blit_fractional_saturating_u8(
                    &mut reference,
                    &mut coverage,
                    data,
                    tile,
                    hit,
                    shape,
                );
            }
        }
        let expected = unpremultiplied_rgba_u8(&reference, &coverage);

        // One-row, odd, table-sized and complete-clip bands.
        for bands in [(0, 1), (100, 1), (0, 16), (usize::MAX, 1)] {
            let mut actual = vec![0; width * height * 4];
            let mut scratch = RgbaBandScratch::default();
            for (hit, tile) in &tiles {
                blit_premultiplied_rgba_in_bands(
                    &mut actual,
                    tile,
                    hit,
                    shape,
                    &mut scratch,
                    bands,
                )
                .unwrap();
            }
            unpremultiply_rgba(&mut actual);
            assert_eq!(actual, expected, "bands {bands:?}");
        }
    }
}

#[test]
fn direct_rgba_preserves_fractional_filtering_and_partial_coverage() {
    let rgb = CpuTile::from_u8_interleaved(
        5,
        4,
        3,
        ColorSpace::Rgb,
        (0..60).map(|value| (value * 37) as u8).collect(),
    )
    .unwrap();
    let rgba =
        CpuTile::from_u8_interleaved(5, 4, 4, ColorSpace::Rgba, [200, 100, 50, 128].repeat(20))
            .unwrap();
    for positions in [
        [(0.25, 0.375), (1.0, 1.0), (0.0, 0.0)],
        [(-1.25, -1.375), (0.0, 0.0), (1.0, 1.0)],
    ] {
        for rgb24 in [false, true] {
            let hits: Vec<_> = positions
                .into_iter()
                .map(|(x, y)| {
                    let mut hit = hit_at(x as i64, y as i64, x, y);
                    hit.cairo_fixed_dest = Some((x, y));
                    hit.cairo_rgb24 = rgb24;
                    hit
                })
                .collect();
            for tiles in [[&rgb, &rgba, &rgb], [&rgba, &rgb, &rgb]] {
                let mut composer = RegionComposer::new(3, 2, tiles[0], true, &hits).unwrap();
                let mut reference = vec![0; 3 * 2 * 3];
                let mut coverage = vec![0.0; 3 * 2];
                let shape = CompositionShape {
                    width: 3,
                    height: 2,
                    channels: 3,
                };
                for (hit, tile) in hits.iter().zip(tiles) {
                    composer.blit(hit, tile).unwrap();
                    if is_alpha_source(tile) {
                        blit_alpha_source_saturating_u8(
                            &mut reference,
                            &mut coverage,
                            tile,
                            hit,
                            shape,
                        )
                        .unwrap();
                    } else {
                        blit_fractional_saturating_u8(
                            &mut reference,
                            &mut coverage,
                            tile.as_u8().unwrap(),
                            tile,
                            hit,
                            shape,
                        );
                    }
                }
                let actual = composer.finish().unwrap().into_rgba().unwrap();
                assert_eq!(
                    actual.as_raw(),
                    &unpremultiplied_rgba_u8(&reference, &coverage)
                );
            }
        }
    }
}

#[test]
fn fractional_u8_composition_matches_cairo_saturate_rounding() {
    let hits = [hit_at(0, -1, 0.0, -0.871_526_272_621_85)];
    let tiles = [tile_2d(CpuTileData::u8(vec![236, 0, 241, 64]), 2, 2)];

    let composed = compose_region_tiles(&hits, &tiles, 2, 1, false).unwrap();

    assert_eq!(composed.data.as_u8().unwrap(), &[241, 55]);
}

#[test]
fn fractional_u8_composition_samples_all_four_bilinear_neighbors() {
    let hits = [hit_at(0, 0, 0.5, 0.5)];
    let tiles = [tile_2d(CpuTileData::u8(vec![0, 64, 128, 255]), 2, 2)];

    let composed = compose_region_tiles(&hits, &tiles, 3, 3, false).unwrap();
    let pixels = composed.data.as_u8().unwrap();

    assert_eq!(pixels[4], 112, "the center averages all four samples");
}

#[test]
fn pixman_bilinear_interpolation_preserves_fused_operation_order() {
    let distance_x = f32::from_bits(0x3f7a_e000);
    let distance_y = f32::from_bits(0x3f7c_7a00);
    let weights = [
        (1.0 - distance_x) * (1.0 - distance_y),
        distance_x * (1.0 - distance_y),
        (1.0 - distance_x) * distance_y,
        distance_x * distance_y,
    ];
    let values = [247, 242, 246, 243].map(|value| unorm8_to_float(value, true));

    assert_eq!(
        pixman_bilinear_interpolate(values, weights).to_bits(),
        0x3f73_ffff
    );
}

#[test]
fn integral_composition_preserves_u16_and_f32_samples() {
    let hits = [hit(0, 0.0), hit(1, 1.0)];
    let u16_tiles = [
        tile(CpuTileData::u16(vec![11]), 1),
        tile(CpuTileData::u16(vec![22]), 1),
    ];
    let f32_tiles = [
        tile(CpuTileData::f32(vec![1.25]), 1),
        tile(CpuTileData::f32(vec![2.5]), 1),
    ];

    let u16_composed = compose_region_tiles(&hits, &u16_tiles, 2, 1, false).unwrap();
    let f32_composed = compose_region_tiles(&hits, &f32_tiles, 2, 1, false).unwrap();

    assert_eq!(u16_composed.data.as_u16().unwrap(), &[11, 22]);
    assert_eq!(f32_composed.data.as_f32().unwrap(), &[1.25, 2.5]);
}

#[test]
fn dense_integral_u8_composition_copies_complete_rows_in_tile_order() {
    let hits = [hit(0, 0.0), hit(2, 2.0)];
    let tiles = [
        tile(CpuTileData::u8(vec![10, 11]), 2),
        tile(CpuTileData::u8(vec![20, 21]), 2),
    ];

    let composed = compose_region_tiles(&hits, &tiles, 4, 1, false).unwrap();

    assert_eq!(composed.data.as_u8().unwrap(), &[10, 11, 20, 21]);
}

#[test]
fn dense_integral_u8_composition_copies_vertical_tiles_in_row_order() {
    let hits = [hit_at(0, 0, 0.0, 0.0), hit_at(0, 2, 0.0, 2.0)];
    let tiles = [
        tile_2d(CpuTileData::u8(vec![10, 11]), 1, 2),
        tile_2d(CpuTileData::u8(vec![20, 21]), 1, 2),
    ];

    let composed = compose_region_tiles(&hits, &tiles, 1, 4, false).unwrap();

    assert_eq!(composed.data.as_u8().unwrap(), &[10, 11, 20, 21]);
}

#[test]
fn composition_rejects_planar_and_mixed_sample_tiles() {
    let planar = Arc::new(CpuTile {
        width: 1,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Planar,
        data: CpuTileData::u8(vec![1]),
    });
    let error = compose_region_tiles(&[hit(0, 0.0)], &[planar], 1, 1, false)
        .expect_err("planar composition must be rejected");
    assert!(error.to_string().contains("planar compositing"));

    let mixed = [
        tile(CpuTileData::u8(vec![1]), 1),
        tile(CpuTileData::u16(vec![2]), 1),
    ];
    let error = compose_region_tiles(&[hit(0, 0.0), hit(1, 1.0)], &mixed, 2, 1, false)
        .expect_err("mixed sample types must be rejected");
    assert!(error.to_string().contains("sample type mismatch"));

    let channel_mismatch = [
        tile(CpuTileData::u8(vec![1]), 1),
        Arc::new(CpuTile {
            width: 1,
            height: 1,
            channels: 2,
            color_space: ColorSpace::Grayscale,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(vec![2, 3]),
        }),
    ];
    let error = compose_region_tiles(&[hit(0, 0.0), hit(1, 1.0)], &channel_mismatch, 2, 1, false)
        .expect_err("mixed channel counts must be rejected");
    assert!(error.to_string().contains("channel count mismatch"));
}

#[test]
fn composition_rejects_truncated_tile_storage_before_blit() {
    let truncated = tile_2d(CpuTileData::u8(vec![1]), 2, 1);

    let error = compose_region_tiles(&[hit(0, 0.0)], &[truncated], 2, 1, false)
        .expect_err("truncated decoded tiles must not reach slice indexing");

    assert!(error.to_string().contains("CpuTile invariant violated"));
}

#[test]
fn dense_row_planner_rejects_gaps_overflow_and_truncated_tiles() {
    let data = [7u8];
    let shape = CompositionShape {
        width: 1,
        height: 1,
        channels: 1,
    };

    let y_overflow_hit = hit_at(0, i64::MAX, 0.0, i64::MAX as f64);
    let y_overflow = DenseIntegralU8Hit {
        hit: &y_overflow_hit,
        data: &data,
        width: 1,
        height: 1,
        row_stride: 1,
    };
    let error = compose_dense_integral_u8_rows(&[y_overflow], shape, 1)
        .expect_err("destination y overflow must fail");
    assert!(error.to_string().contains("destination y overflow"));

    let x_overflow_hit = hit_at(i64::MAX, 0, i64::MAX as f64, 0.0);
    let x_overflow = DenseIntegralU8Hit {
        hit: &x_overflow_hit,
        data: &data,
        width: 1,
        height: 1,
        row_stride: 1,
    };
    let error = compose_dense_integral_u8_rows(&[x_overflow], shape, 1)
        .expect_err("destination x overflow must fail");
    assert!(error.to_string().contains("destination x overflow"));

    let valid_hit = hit(0, 0.0);
    let truncated = DenseIntegralU8Hit {
        hit: &valid_hit,
        data: &[],
        width: 1,
        height: 1,
        row_stride: 1,
    };
    let error = compose_dense_integral_u8_rows(&[truncated], shape, 1)
        .expect_err("truncated decoded row must fail");
    assert!(error.to_string().contains("source row exceeds"));

    let complete = DenseIntegralU8Hit {
        hit: &valid_hit,
        data: &data,
        width: 1,
        height: 1,
        row_stride: 1,
    };
    let error = compose_dense_integral_u8_rows(&[complete], shape, 2)
        .expect_err("incorrect output accounting must fail");
    assert!(error.to_string().contains("produced 1 samples, expected 2"));

    let gap_hit = hit(1, 1.0);
    let gap = DenseIntegralU8Hit {
        hit: &gap_hit,
        data: &data,
        width: 1,
        height: 1,
        row_stride: 1,
    };
    let gap_shape = CompositionShape {
        width: 2,
        height: 1,
        channels: 1,
    };
    assert!(compose_dense_integral_u8_rows(&[gap], gap_shape, 2)
        .unwrap()
        .is_none());
}

#[test]
fn dense_row_planner_checks_source_offset_and_copy_length_arithmetic() {
    let data = [0u8; 1];
    let offset_hit = hit_at(0, -2, 0.0, -2.0);
    let offset_entry = DenseIntegralU8Hit {
        hit: &offset_hit,
        data: &data,
        width: 1,
        height: 3,
        row_stride: usize::MAX,
    };
    let unit_shape = CompositionShape {
        width: 1,
        height: 1,
        channels: 1,
    };
    let error = compose_dense_integral_u8_rows(&[offset_entry], unit_shape, 1)
        .expect_err("source offset overflow must fail");
    assert!(error.to_string().contains("source offset overflow"));

    let copy_hit = hit(0, 0.0);
    let copy_entry = DenseIntegralU8Hit {
        hit: &copy_hit,
        data: &data,
        width: 2,
        height: 1,
        row_stride: usize::MAX,
    };
    let huge_channel_shape = CompositionShape {
        width: 2,
        height: 1,
        channels: usize::MAX,
    };
    let error = compose_dense_integral_u8_rows(&[copy_entry], huge_channel_shape, 0)
        .expect_err("copy length overflow must fail");
    assert!(error.to_string().contains("copy length overflow"));

    let clipped_hit = hit_at(-2, 0, -2.0, 0.0);
    let clipped_entry = DenseIntegralU8Hit {
        hit: &clipped_hit,
        data: &data,
        width: 3,
        height: 1,
        row_stride: 1,
    };
    let error = compose_dense_integral_u8_rows(&[clipped_entry], huge_channel_shape, 0)
        .expect_err("source x sample offset overflow must fail");
    assert!(error.to_string().contains("source x byte offset overflow"));

    let end_hit = hit_at(0, -1, 0.0, -1.0);
    let end_entry = DenseIntegralU8Hit {
        hit: &end_hit,
        data: &data,
        width: 2,
        height: 2,
        row_stride: usize::MAX - 1,
    };
    let two_pixel_shape = CompositionShape {
        width: 2,
        height: 1,
        channels: 1,
    };
    let error = compose_dense_integral_u8_rows(&[end_entry], two_pixel_shape, 2)
        .expect_err("source row end overflow must fail");
    assert!(error.to_string().contains("source end overflow"));
}

#[test]
fn rgb_crop_rejects_wrong_sample_type_and_output_size_overflow() {
    let wrong_type = CpuTile {
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u16(vec![0; 3]),
    };
    let error = crop_rgb_interleaved_u8_buffer(&wrong_type, 0, 0, 1, 1)
        .expect_err("U16 RGB crop must fail");
    assert!(error.to_string().contains("expects U8 source data"));

    let oversized = CpuTile {
        width: u32::MAX,
        height: u32::MAX,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(Vec::new()),
    };
    let error = crop_rgb_interleaved_u8_buffer(&oversized, 0, 0, u32::MAX, u32::MAX)
        .expect_err("oversized crop allocation must fail");
    assert!(error
        .to_string()
        .contains("destination byte count overflow"));
}

#[test]
fn compositor_boundaries_report_missing_alpha_and_output_size_overflow() {
    let fractional_hit = hit(0, 0.5);
    let source = tile(CpuTileData::u8(vec![1]), 1);
    let mut output = CpuTileData::u8(vec![0]);
    let shape = CompositionShape {
        width: 1,
        height: 1,
        channels: 1,
    };
    let error = blit_region_tile(&mut output, None, &source, &fractional_hit, shape)
        .expect_err("fractional blit without alpha state must fail");
    assert!(error.to_string().contains("alpha buffer missing"));

    let hits = [hit(0, 0.0)];
    let tiles = [source];
    let error = try_compose_dense_integral_u8_region(
        &hits,
        &tiles,
        u32::MAX,
        u32::MAX,
        3,
        &ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
    )
    .expect_err("oversized dense output must fail before allocation");
    assert!(error.to_string().contains("region output size overflow"));

    assert!(checked_total_samples(u32::MAX, u32::MAX, u16::MAX)
        .expect_err("oversized sample count must fail")
        .to_string()
        .contains("region sample count overflow"));
}

#[test]
fn metadata_probe_uses_row_major_order_for_irregular_tiles() {
    let mut tiles = std::collections::HashMap::new();
    tiles.insert((5, 2), TileEntry::new((5.0, 2.0), (1, 1)));
    tiles.insert((9, 1), TileEntry::new((9.0, 1.0), (1, 1)));
    tiles.insert((3, 1), TileEntry::new((3.0, 1.0), (1, 1)));
    let layout = TileLayout::Irregular {
        tile_advance: (1.0, 1.0),
        extra_tiles: (0, 0, 0, 0),
        tiles,
    };

    assert_eq!(metadata_probe_coordinate(&layout), Some((3, 1)));
}

#[test]
fn bounded_region_batches_preserve_pixels_and_do_not_load_the_whole_region() {
    let source = StreamingSource {
        dataset: crate::test_support::regular_rgb_dataset_for_test(
            DatasetId::new(78),
            "s0",
            "ser0",
            crate::test_support::RegularLevelForTest {
                dimensions: (16, 1),
                tile_width: 2,
                tile_height: 1,
                tiles_across: 8,
                tiles_down: 1,
            },
        ),
        single_reads: AtomicUsize::new(0),
        batch_reads: AtomicUsize::new(0),
    };
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (16, 1));
    let region = composite_region_from_source_in_batches(&source, None, &req, 16, 3).unwrap();
    let expected: Vec<_> = (1..=8).flat_map(|v| vec![v; 6]).collect();
    assert_eq!(region.as_u8().unwrap(), expected);
    assert_eq!(
        source.batch_reads.load(Ordering::SeqCst),
        2,
        "after the first tile, use two three-tile batches and a final single tile"
    );
    assert_eq!(source.single_reads.load(Ordering::SeqCst), 8);
}

/// Two vertically stacked 4x4 tiles that overlap by 1.5 rows, as stitched
/// Ventana and MIRAX tiles do. Each tile is one solid value.
struct OverlappingRowsSource {
    dataset: Dataset,
}

impl OverlappingRowsSource {
    fn new(offset_x: f64) -> Self {
        let mut dataset = crate::test_support::regular_rgb_dataset_for_test(
            DatasetId::new(79),
            "s0",
            "ser0",
            crate::test_support::RegularLevelForTest {
                dimensions: (8, 8),
                tile_width: 4,
                tile_height: 4,
                tiles_across: 1,
                tiles_down: 2,
            },
        );
        let tiles = [(0, 0), (0, 1)]
            .into_iter()
            .map(|key| (key, TileEntry::new((offset_x, 0.0), (4, 4))))
            .collect();
        dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::Irregular {
            tile_advance: (4.0, 2.5),
            extra_tiles: (1, 0, 0, 0),
            tiles,
        };
        Self { dataset }
    }
}

impl SlideReader for OverlappingRowsSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        let value = if req.row == 0 { 10 } else { 200 };
        CpuTile::from_u8_interleaved(4, 4, 3, ColorSpace::Rgb, vec![value; 4 * 4 * 3])
    }
}

#[test]
fn overlapping_irregular_tiles_keep_the_lower_tile_like_openslide() {
    // OpenSlide paints tilemaps from the bottom-right tile back to the top-left
    // under SATURATE, so the first-painted lower tile owns the overlap rows
    // 2.5..4. Painting top-down let the upper tile win instead.
    for offset_x in [0.0, 0.25] {
        let source = OverlappingRowsSource::new(offset_x);
        let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (4, 6));
        let region =
            composite_fractional_region_from_source(&source, None, &req, (0.0, 0.0), 64).unwrap();
        let rgba = region.into_rgba().unwrap().into_raw();
        let red_at = |x: usize, y: usize| rgba[(y * 4 + x) * 4];
        // Column 2 is fully inside both tiles for either horizontal offset.
        assert_eq!(red_at(2, 1), 10, "upper tile alone, offset_x={offset_x}");
        assert_eq!(red_at(2, 3), 200, "overlap belongs to the lower tile");
        assert_eq!(red_at(2, 5), 200, "lower tile alone, offset_x={offset_x}");
    }
}
