use super::*;
use std::collections::HashMap;
use wsi_rs::{TileEntry, TileLayout};

#[test]
fn banded_reads_preserve_fractional_pixels_gaps_and_edges() {
    use wsi_rs::{
        AxesShape, CpuTile, Dataset, DatasetId, SampleType, Scene, Series, SlideReader, TileRequest,
    };

    struct Pattern(Dataset);
    impl SlideReader for Pattern {
        fn dataset(&self) -> &Dataset {
            &self.0
        }
        fn read_tile_cpu(&self, request: &TileRequest) -> Result<CpuTile, WsiError> {
            let mut data = Vec::with_capacity(128 * 128 * 3);
            for y in 0..128 {
                for x in 0..128 {
                    data.extend_from_slice(&[
                        (request.col * 37 + x) as u8,
                        (request.row * 29 + y) as u8,
                        (x + y) as u8,
                    ]);
                }
            }
            CpuTile::from_u8_interleaved(128, 128, 3, ColorSpace::Rgb, data)
        }
    }

    let layouts = [
        TileLayout::Regular {
            tile_width: 128,
            tile_height: 128,
            tiles_across: 4,
            tiles_down: 8,
        },
        TileLayout::Irregular {
            tile_advance: (128.0, 128.0),
            extra_tiles: (0, 0, 0, 0),
            tiles: (0..8)
                .flat_map(|row| (0..4).map(move |col| (col, row)))
                .filter(|position| *position != (1, 3))
                .map(|position| (position, TileEntry::new((0.0, 0.0), (128, 128))))
                .collect(),
        },
        TileLayout::Irregular {
            tile_advance: (130.25, 126.5),
            extra_tiles: (1, 1, 1, 1),
            tiles: (0..8)
                .flat_map(|row| (0..4).map(move |col| (col, row)))
                .filter(|position| *position != (1, 3))
                .map(|(col, row)| {
                    (
                        (col, row),
                        TileEntry::new(
                            ((col % 2) as f64 * 0.25, (row % 2) as f64 * 0.5),
                            (128, 128),
                        ),
                    )
                })
                .collect(),
        },
    ];
    for layout in layouts {
        let level = Level::new((530, 1030), 1.0, layout);
        let dataset = Dataset::new(
            DatasetId::new(1),
            vec![Scene::new(
                "pattern",
                vec![Series::new(
                    "rgb",
                    AxesShape::default(),
                    vec![level],
                    SampleType::Uint8,
                    vec![],
                )],
            )],
        );
        let slide = Slide::from_source_with_cache_bytes(Box::new(Pattern(dataset)), 0);
        let level = &slide.dataset().scenes[0].series[0].levels[0];
        let request = RegionRequest::new(0, 0, 0, (-13, -7), (541, 1047));
        for offset in [(0.0, 0.0), (0.25, 0.5)] {
            let tile = slide.read_region_subpixel(&request, offset).unwrap();
            let opaque = !matches!(tile.color_space(), ColorSpace::Rgba);
            let mut expected = crate::pixels::tile_to_premultiplied_argb(tile).unwrap();
            clear_uncovered_pixels(
                level,
                request.origin_px,
                offset,
                request.size_px,
                &mut expected,
                opaque,
            )
            .unwrap();
            std::thread::scope(|scope| {
                for _ in 0..4 {
                    scope.spawn(|| {
                        let mut actual = vec![u32::MAX; expected.len()];
                        read_region_into(&slide, level, &request, offset, &mut actual).unwrap();
                        let mismatch = actual.iter().zip(&expected).position(|(a, b)| a != b);
                        assert_eq!(
                            mismatch,
                            None,
                            "band seams at offset {offset:?}: {:?}",
                            mismatch.map(|i| (i % 541, i / 541, actual[i], expected[i]))
                        );
                    });
                }
            });
        }
    }
}

#[test]
fn cached_dense_reads_match_the_composed_path() {
    use wsi_rs::{
        AxesShape, CpuTile, Dataset, DatasetId, SampleType, Scene, Series, SlideReader, TileRequest,
    };

    struct Pattern(Dataset);
    impl SlideReader for Pattern {
        fn dataset(&self) -> &Dataset {
            &self.0
        }
        fn read_tile_cpu(&self, request: &TileRequest) -> Result<CpuTile, WsiError> {
            let mut data = Vec::with_capacity(128 * 128 * 3);
            for y in 0..128 {
                for x in 0..128 {
                    data.extend_from_slice(&[
                        (request.col * 37 + x) as u8,
                        (request.row * 29 + y) as u8,
                        (x ^ y) as u8,
                    ]);
                }
            }
            CpuTile::from_u8_interleaved(128, 128, 3, ColorSpace::Rgb, data)
        }
    }

    for irregular in [false, true] {
        let open = |cache_bytes| {
            let layout = if irregular {
                TileLayout::Irregular {
                    tile_advance: (128.0, 128.0),
                    extra_tiles: (0, 0, 0, 0),
                    tiles: (0..8)
                        .flat_map(|row| (0..4).map(move |col| (col, row)))
                        .filter(|position| *position != (1, 3))
                        .map(|position| (position, TileEntry::new((0.0, 0.0), (128, 128))))
                        .collect(),
                }
            } else {
                TileLayout::Regular {
                    tile_width: 128,
                    tile_height: 128,
                    tiles_across: 4,
                    tiles_down: 8,
                }
            };
            let dataset = Dataset::new(
                DatasetId::new(2),
                vec![Scene::new(
                    "pattern",
                    vec![Series::new(
                        "rgb",
                        AxesShape::default(),
                        vec![Level::new((500, 1024), 1.0, layout)],
                        SampleType::Uint8,
                        vec![],
                    )],
                )],
            );
            Slide::from_source_with_cache_bytes(Box::new(Pattern(dataset)), cache_bytes)
        };
        let reference = open(0);
        let cached = open(64 << 20);
        let level = &cached.dataset().scenes[0].series[0].levels[0];
        let dense = RegionRequest::new(0, 0, 0, (5, 9), (250, 300));
        let small_dense = RegionRequest::new(0, 0, 0, (5, 9), (250, 250));
        let gap_origin = if irregular { (100, 350) } else { (500, 350) };
        let gap = RegionRequest::new(0, 0, 0, gap_origin, (200, 100));
        let edge = RegionRequest::new(0, 0, 0, (480, 100), (32, 100));
        for (request, offset, dense_hits) in [
            (&dense, (0.0, 0.0), true),
            (&small_dense, (0.0, 0.0), true),
            (&edge, (0.0, 0.0), true),
            (&gap, (0.0, 0.0), false),
            (&dense, (0.25, 0.5), false),
        ] {
            let tile = reference.read_region_subpixel(request, offset).unwrap();
            let opaque = !matches!(tile.color_space(), ColorSpace::Rgba);
            let mut expected = crate::pixels::tile_to_premultiplied_argb(tile).unwrap();
            clear_uncovered_pixels(
                level,
                request.origin_px,
                offset,
                request.size_px,
                &mut expected,
                opaque,
            )
            .unwrap();
            // The first read decodes and caches; the second reuses cached tiles.
            for _ in 0..2 {
                let mut actual = vec![u32::MAX; expected.len()];
                read_region_into(&cached, level, request, offset, &mut actual).unwrap();
                assert!(actual == expected, "{request:?} at {offset:?}");
            }
            let mut probe = vec![u32::MAX; expected.len()];
            assert_eq!(
                cached
                    .read_cached_region_argb32_into(request, offset, &mut probe)
                    .unwrap(),
                dense_hits,
                "{request:?} at {offset:?}"
            );
        }
    }
}

#[test]
fn banding_does_not_bypass_complete_region_limits() {
    use wsi_rs::{SlideLimits, SlideOpenOptions};
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/jp2k/rgb_nomct.j2k");
    for (limits, resource) in [
        (
            SlideLimits::default()
                .with_region_pixels(256 * 1024)
                .unwrap(),
            "region pixels",
        ),
        (
            SlideLimits::default()
                .with_region_rgba_bytes(1024 * 1024)
                .unwrap(),
            "region RGBA output",
        ),
    ] {
        let slide =
            Slide::open_with_options(&path, SlideOpenOptions::default().with_limits(limits))
                .unwrap();
        let level = &slide.dataset().scenes[0].series[0].levels[0];
        let request = RegionRequest::new(0, 0, 0, (0, 0), (512, 513));
        let mut pixels = vec![u32::MAX; 512 * 513];
        let error = read_region_into(&slide, level, &request, (0.0, 0.0), &mut pixels).unwrap_err();
        assert!(
            matches!(error, WsiError::ResourceLimit { resource: actual, .. } if actual == resource)
        );
    }
}

#[test]
fn opaque_irregular_gaps_are_canonical_transparent_argb() {
    let level = Level::new(
        (3, 1),
        1.0,
        TileLayout::Irregular {
            tile_advance: (2.0, 1.0),
            extra_tiles: (0, 0, 0, 0),
            tiles: HashMap::from([((0, 0), TileEntry::new((0.0, 0.0), (1, 1)))]),
        },
    );
    let mut pixels = [0xff11_2233; 3];

    clear_uncovered_pixels(&level, (0, 0), (0.0, 0.0), (3, 1), &mut pixels, true)
        .expect("mark irregular coverage");

    assert_eq!(pixels, [0xff11_2233, 0, 0]);
}

#[test]
fn opaque_irregular_coverage_follows_the_subpixel_origin() {
    // The tile starts at level x 1.5; read from origin 0.75 it covers output
    // pixels 0 and 1, not the pixels 1 and 2 its whole-pixel placement implies.
    let level = Level::new(
        (4, 1),
        1.0,
        TileLayout::Irregular {
            tile_advance: (4.0, 1.0),
            extra_tiles: (0, 0, 0, 0),
            tiles: HashMap::from([((0, 0), TileEntry::new((1.5, 0.0), (1, 1)))]),
        },
    );
    let mut pixels = [0xff11_2233; 3];

    clear_uncovered_pixels(&level, (0, 0), (0.75, 0.0), (3, 1), &mut pixels, true)
        .expect("mark irregular coverage");

    assert_eq!(pixels, [0xff11_2233, 0xff11_2233, 0]);
}

#[test]
fn irregular_alpha_output_keeps_the_composed_coverage() {
    let level = Level::new(
        (3, 1),
        1.0,
        TileLayout::Irregular {
            tile_advance: (2.0, 1.0),
            extra_tiles: (0, 0, 0, 0),
            tiles: HashMap::from([((0, 0), TileEntry::new((0.25, 0.0), (1, 1)))]),
        },
    );
    // Partial edge coverage from the tile, then an untouched transparent gap.
    let mut pixels = [0xbf11_2233, 0x3f04_0809, 0];

    clear_uncovered_pixels(&level, (0, 0), (0.5, 0.0), (3, 1), &mut pixels, false)
        .expect("alpha output needs no coverage pass");

    assert_eq!(pixels, [0xbf11_2233, 0x3f04_0809, 0]);
}

#[test]
fn regular_regions_clear_pixels_outside_level_extent() {
    let level = Level::new(
        (1, 1),
        1.0,
        TileLayout::WholeLevel {
            width: 1,
            height: 1,
            virtual_tile_width: 1,
            virtual_tile_height: 1,
        },
    );
    let mut pixels = [0xff01_0203; 3];

    clear_uncovered_pixels(&level, (-1, 0), (0.0, 0.0), (3, 1), &mut pixels, true)
        .expect("clip regular coverage");

    assert_eq!(pixels, [0, 0xff01_0203, 0]);
}

#[test]
fn coverage_rejects_a_destination_with_the_wrong_length() {
    let level = Level::new(
        (1, 1),
        1.0,
        TileLayout::WholeLevel {
            width: 1,
            height: 1,
            virtual_tile_width: 1,
            virtual_tile_height: 1,
        },
    );

    let error = clear_uncovered_pixels(&level, (0, 0), (0.0, 0.0), (1, 1), &mut [], true)
        .expect_err("coverage destination length must be exact");

    assert!(error.to_string().contains("has 0 pixels, expected 1"));
}
