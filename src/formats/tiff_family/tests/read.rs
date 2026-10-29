use super::super::*;
use super::fixtures::*;
use crate::core::types::{ColorSpace, CpuTile, TileRequest};
use crate::formats::tiff_family::container::tags;
use crate::formats::tiff_family::test_support::{build_tiff_tree, SyntheticIfd, SyntheticTag};

#[test]
fn open_produces_slide_reader() {
    let file = build_ndpi_tiff(&[(1024, 768, 40.0)]);
    let backend = TiffFamilyBackend::new();

    // First probe, then open (the normal flow)
    let probe_result = backend.probe(file.path()).unwrap();
    assert!(probe_result.detected);

    let source = backend.open(file.path()).unwrap();
    let dataset = source.dataset();

    assert_eq!(dataset.scenes.len(), 1);
    let series = &dataset.scenes[0].series[0];
    assert_eq!(series.levels.len(), 9);
    assert_eq!(series.levels[0].dimensions, (1024, 768));
    assert_eq!(series.levels[1].dimensions, (512, 384));
    assert_eq!(series.levels[2].dimensions, (256, 192));
    assert_eq!(series.levels[8].dimensions, (4, 3));
}

#[test]
fn open_without_prior_probe_works() {
    let file = build_ndpi_tiff(&[(512, 384, 20.0)]);
    let backend = TiffFamilyBackend::new();

    // Skip probe and call open() directly
    let source = backend.open(file.path()).unwrap();
    let dataset = source.dataset();

    assert_eq!(dataset.scenes.len(), 1);
    assert_eq!(dataset.scenes[0].series[0].levels[0].dimensions, (512, 384));
}

// ── TiledIfd end-to-end test ─────────────────────────────────

#[test]
fn aperio_open_and_read_tile() {
    let file = build_aperio_tiff(64, 64);
    let backend = TiffFamilyBackend::new();

    let source = backend.open(file.path()).unwrap();
    let dataset = source.dataset();
    assert_eq!(dataset.scenes.len(), 1);
    assert_eq!(dataset.scenes[0].series[0].levels[0].dimensions, (64, 64));

    // Tile (0, 0) must decode as JPEG
    let req = crate::core::types::TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let tile = source.read_tile_cpu(&req).unwrap();
    assert_eq!(tile.width, 64);
    assert_eq!(tile.height, 64);
    assert_eq!(tile.channels, 3);
    assert_eq!(tile.color_space, ColorSpace::Rgb);
}

#[test]
fn generic_tiff_detected_as_fallback() {
    let file = build_generic_tiled_tiff(256, 256);
    let backend = TiffFamilyBackend::new();
    let result = backend.probe(file.path()).unwrap();
    assert!(result.detected);
    assert_eq!(result.vendor, "generic-tiff");
}

#[test]
fn generic_tiff_open_and_read_tile() {
    let file = build_generic_tiled_tiff(64, 64);
    let backend = TiffFamilyBackend::new();

    let source = backend.open(file.path()).unwrap();
    let dataset = source.dataset();
    assert_eq!(dataset.scenes.len(), 1);
    assert_eq!(dataset.properties.vendor(), Some("generic-tiff"));

    let req = crate::core::types::TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let tile = source.read_tile_cpu(&req).unwrap();
    assert_eq!(tile.width, 64);
    assert_eq!(tile.height, 64);
    assert_eq!(tile.channels, 3);
}

#[test]
fn generic_planar_stripped_rgb_tiff_opens_and_reads_synthetic_edge_tile() {
    let file = build_planar_stripped_rgb_tiff(260, 258, 128);
    let backend = TiffFamilyBackend::new();

    let probe = backend.probe(file.path()).unwrap();
    assert!(probe.detected);
    assert_eq!(probe.vendor, "generic-tiff");

    let source = backend.open(file.path()).unwrap();
    let level = &source.dataset().scenes[0].series[0].levels[0];
    assert_eq!(level.dimensions, (260, 258));
    assert!(matches!(
        level.tile_layout,
        crate::core::types::TileLayout::Regular {
            tile_width: 256,
            tile_height: 256,
            tiles_across: 2,
            tiles_down: 2,
        }
    ));

    let tile = source
        .read_tile_cpu(&crate::core::types::TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: crate::core::types::PlaneSelection::default().into(),
            col: 1,
            row: 1,
        })
        .unwrap();
    assert_eq!((tile.width, tile.height), (4, 2));
    assert_eq!(tile.channels, 3);
    assert_eq!(tile.color_space, ColorSpace::Rgb);
    let pixels = tile.data.as_u8().unwrap();
    assert_eq!(&pixels[..3], &[0, 0, 0]);
    assert_eq!(&pixels[pixels.len() - 3..], &[3, 1, 4]);
}

#[test]
fn uncompressed_tiled_tiff_le_read() {
    let file = build_uncompressed_tiled_tiff(8, 8, false);
    let backend = TiffFamilyBackend::new();
    let source = backend.open(file.path()).unwrap();
    let req = crate::core::types::TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let tile = source.read_tile_cpu(&req).unwrap();
    assert_eq!(tile.width, 8);
    assert_eq!(tile.height, 8);
    assert_eq!(tile.channels, 3);
    // Verify test pattern: pixel (0,0) = (0, 0, 128)
    let data = tile.data.as_u8().unwrap();
    assert_eq!(data[0], 0); // R
    assert_eq!(data[1], 0); // G
    assert_eq!(data[2], 128); // B
                              // pixel (1,0) = (1, 0, 128)
    assert_eq!(data[3], 1);
    assert_eq!(data[4], 0);
    assert_eq!(data[5], 128);
}

#[test]
fn uncompressed_tiled_tiff_be_read() {
    // Big-endian TIFF with uncompressed RGB u8 data.
    // u8 data is endian-neutral, so this tests that the IFD parsing
    // and tag decoding handles big-endian correctly.
    let file = build_uncompressed_tiled_tiff(8, 8, true);
    let backend = TiffFamilyBackend::new();
    let source = backend.open(file.path()).unwrap();
    let req = crate::core::types::TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let tile = source.read_tile_cpu(&req).unwrap();
    assert_eq!(tile.width, 8);
    assert_eq!(tile.height, 8);
    let data = tile.data.as_u8().unwrap();
    assert_eq!(data[0], 0);
    assert_eq!(data[1], 0);
    assert_eq!(data[2], 128);
}

#[test]
fn u16_grayscale_big_endian_decode() {
    let file = build_u16_grayscale_tiff(4, 4, true);
    let backend = TiffFamilyBackend::new();
    let source = backend.open(file.path()).unwrap();
    let req = crate::core::types::TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let tile = source.read_tile_cpu(&req).unwrap();
    assert_eq!(tile.width, 4);
    assert_eq!(tile.height, 4);
    assert_eq!(tile.channels, 1);
    let data = tile.data.as_u16().unwrap();
    // pixel (0,0) = 0, pixel (1,0) = 1, pixel (0,1) = 4
    assert_eq!(data[0], 0);
    assert_eq!(data[1], 1);
    assert_eq!(data[4], 4);
}

#[test]
fn u16_grayscale_little_endian_decode() {
    let file = build_u16_grayscale_tiff(4, 4, false);
    let backend = TiffFamilyBackend::new();
    let source = backend.open(file.path()).unwrap();
    let req = crate::core::types::TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let tile = source.read_tile_cpu(&req).unwrap();
    let data = tile.data.as_u16().unwrap();
    assert_eq!(data[0], 0);
    assert_eq!(data[1], 1);
    assert_eq!(data[4], 4);
}

#[test]
fn min_is_white_inversion() {
    let file = build_min_is_white_tiff(8, 8);
    let backend = TiffFamilyBackend::new();
    let source = backend.open(file.path()).unwrap();
    let req = crate::core::types::TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let tile = source.read_tile_cpu(&req).unwrap();
    assert_eq!(tile.channels, 1);
    assert_eq!(tile.color_space, ColorSpace::Grayscale);
    let data = tile.data.as_u8().unwrap();
    // In MinIsWhite, raw 0 = white → inverted to 255
    assert_eq!(data[0], 255); // pixel (0,0): raw=0 → 255
    assert_eq!(data[1], 254); // pixel (1,0): raw=1 → 254
    assert_eq!(data[7], 248); // pixel (7,0): raw=7 → 248
}

// ── Strip-only TIFF ──────────────────────────────────────────

fn read_level0_tile(source: &dyn SlideReader, col: i64, row: i64) -> Result<CpuTile, WsiError> {
    source.read_tile_cpu(&TileRequest::new(0usize, 0usize, 0, col, row))
}

/// Asserts every 256-pixel tile of a `width` x `height` level holds the
/// strip pattern with `channels` samples per pixel.
fn assert_tiles_match_strip_pattern(
    source: &dyn SlideReader,
    width: u32,
    height: u32,
    channels: u16,
    color_space: ColorSpace,
) {
    for row in 0..height.div_ceil(256) {
        for col in 0..width.div_ceil(256) {
            let tile = read_level0_tile(source, i64::from(col), i64::from(row)).unwrap();
            let (x, y) = (col * 256, row * 256);
            let (tile_width, tile_height) = (256.min(width - x), 256.min(height - y));
            assert_eq!((tile.width, tile.height), (tile_width, tile_height));
            assert_eq!(tile.channels, channels);
            assert_eq!(tile.color_space, color_space);
            assert_eq!(
                tile.data.as_u8().unwrap(),
                strip_pattern_window(x, y, tile_width, tile_height, u32::from(channels)),
                "tile ({col}, {row})"
            );
        }
    }
}

#[test]
fn lzw_rgb_strips_with_predictor_read_exactly_across_strip_and_tile_edges() {
    // Seven-row strips put a strip across the tile boundary at row 256.
    let (width, height) = (300, 270);
    let file = build_strip_only_tiff(
        width,
        height,
        3,
        7,
        vec![
            SyntheticTag::short(tags::COMPRESSION, 5),
            SyntheticTag::short(tags::PHOTOMETRIC, 2),
            SyntheticTag::short(tags::PREDICTOR, 2),
        ],
        |_, raw| tiff_lzw(&horizontal_difference(raw, width, 3)),
    );
    let backend = TiffFamilyBackend::new();
    assert_eq!(backend.probe(file.path()).unwrap().vendor, "generic-tiff");
    let source = backend.open(file.path()).unwrap();
    assert_eq!(
        source.dataset().scenes[0].series[0].levels[0].dimensions,
        (300, 270)
    );

    assert_tiles_match_strip_pattern(source.as_ref(), width, height, 3, ColorSpace::Rgb);
}

#[test]
fn deflate_grayscale_strips_read_as_one_channel() {
    let (width, height) = (260, 258);
    let file = build_strip_only_tiff(
        width,
        height,
        1,
        16,
        vec![
            SyntheticTag::short(tags::COMPRESSION, 8),
            SyntheticTag::short(tags::PHOTOMETRIC, 1),
        ],
        |_, raw| tiff_deflate(raw),
    );
    let source = TiffFamilyBackend::new().open(file.path()).unwrap();

    assert_tiles_match_strip_pattern(source.as_ref(), width, height, 1, ColorSpace::Grayscale);
}

#[test]
fn uncompressed_rgb_strips_read_exact_tile_windows() {
    let (width, height) = (300, 260);
    let file = build_strip_only_tiff(
        width,
        height,
        3,
        5,
        vec![
            SyntheticTag::short(tags::COMPRESSION, 1),
            SyntheticTag::short(tags::PHOTOMETRIC, 2),
        ],
        |_, raw| raw.to_vec(),
    );
    let source = TiffFamilyBackend::new().open(file.path()).unwrap();

    assert_tiles_match_strip_pattern(source.as_ref(), width, height, 3, ColorSpace::Rgb);
}

#[test]
fn min_is_white_strips_are_inverted() {
    let (width, height) = (20, 9);
    let file = build_strip_only_tiff(
        width,
        height,
        1,
        4,
        vec![
            SyntheticTag::short(tags::COMPRESSION, 5),
            SyntheticTag::short(tags::PHOTOMETRIC, 0),
        ],
        |_, raw| tiff_lzw(raw),
    );
    let source = TiffFamilyBackend::new().open(file.path()).unwrap();

    let tile = read_level0_tile(source.as_ref(), 0, 0).unwrap();
    assert_eq!(tile.color_space, ColorSpace::Grayscale);
    let inverted: Vec<u8> = strip_pattern_window(0, 0, width, height, 1)
        .into_iter()
        .map(|sample| 255 - sample)
        .collect();
    assert_eq!(tile.data.as_u8().unwrap(), inverted);
}

#[test]
fn jpeg_ycbcr_strips_decode_to_rgb_including_a_short_last_strip() {
    // 300 rows in 16-row strips: strip 16 starts tile row 1, strip 18 holds
    // the last 12 rows.
    let (width, height, rows_per_strip) = (40, 300, 16);
    let strip_color = |index: usize| {
        let index = index as u8;
        [index * 13, 200 - index * 9, 60 + index * 7]
    };
    let file = build_strip_only_tiff(
        width,
        height,
        3,
        rows_per_strip,
        vec![
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::short(tags::PHOTOMETRIC, 6),
        ],
        |index, raw| {
            let rows = (raw.len() / (width as usize * 3)) as u32;
            solid_rgb_jpeg(width, rows, strip_color(index))
        },
    );
    let source = TiffFamilyBackend::new().open(file.path()).unwrap();

    for (tile_row, tile_height) in [(0u32, 256u32), (1, 44)] {
        let tile = read_level0_tile(source.as_ref(), 0, i64::from(tile_row)).unwrap();
        assert_eq!((tile.width, tile.height), (width, tile_height));
        assert_eq!(tile.color_space, ColorSpace::Rgb);
        let pixels = tile.data.as_u8().unwrap();
        for y in 0..tile_height {
            let expected = strip_color(((tile_row * 256 + y) / rows_per_strip) as usize);
            let first = &pixels[(y * width * 3) as usize..][..3];
            assert!(
                first
                    .iter()
                    .zip(expected)
                    .all(|(&got, want)| got.abs_diff(want) <= 4),
                "row {} is {first:?}, expected about {expected:?}",
                tile_row * 256 + y
            );
        }
    }
}

#[test]
fn strip_only_tiff_over_the_decoded_image_limit_opens() {
    // 9000 x 6000 RGB is 162 MB decoded, over the 128 MiB image limit, but
    // each 16-row strip decodes to 432 KB. Open reads no strip pixels.
    let (width, height, rows_per_strip) = (9000u32, 6000u32, 16u32);
    let strips = vec![vec![0u8; 4]; height.div_ceil(rows_per_strip) as usize];
    let byte_counts = vec![4; strips.len()];
    let file = build_tiff_tree(&[SyntheticIfd {
        tags: vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, width),
            SyntheticTag::long(tags::IMAGE_LENGTH, height),
            SyntheticTag::long_array(tags::BITS_PER_SAMPLE, &[8, 8, 8]),
            SyntheticTag::short(tags::COMPRESSION, 5),
            SyntheticTag::short(tags::PHOTOMETRIC, 2),
            SyntheticTag::short(tags::SAMPLES_PER_PIXEL, 3),
            SyntheticTag::long(tags::ROWS_PER_STRIP, rows_per_strip),
            SyntheticTag::blob_offsets(tags::STRIP_OFFSETS, strips),
            SyntheticTag::long_array(tags::STRIP_BYTE_COUNTS, &byte_counts),
        ],
        sub_ifds: Vec::new(),
    }]);

    let source = TiffFamilyBackend::new().open(file.path()).unwrap();

    assert_eq!(
        source.dataset().scenes[0].series[0].levels[0].dimensions,
        (9000, 6000)
    );
    // The strips are not valid LZW: a read fails instead of returning black.
    let err = read_level0_tile(source.as_ref(), 3, 2).unwrap_err();
    assert!(err.to_string().contains("strip 32"), "{err}");
}

#[test]
fn truncated_uncompressed_strip_fails_the_tiles_that_cover_it() {
    let (width, height) = (16, 10);
    let file = build_strip_only_tiff(
        width,
        height,
        3,
        5,
        vec![
            SyntheticTag::short(tags::COMPRESSION, 1),
            SyntheticTag::short(tags::PHOTOMETRIC, 2),
        ],
        |index, raw| {
            let keep = if index == 1 { raw.len() - 3 } else { raw.len() };
            raw[..keep].to_vec()
        },
    );
    let source = TiffFamilyBackend::new().open(file.path()).unwrap();

    let err = read_level0_tile(source.as_ref(), 0, 0).unwrap_err();
    assert!(
        err.to_string()
            .contains("uncompressed strip 1 holds 237 bytes"),
        "{err}"
    );
}

#[test]
fn grayscale_strip_regions_compose_across_tiles_through_slide() {
    let (width, height) = (260, 258);
    let file = build_strip_only_tiff(
        width,
        height,
        1,
        9,
        vec![
            SyntheticTag::short(tags::COMPRESSION, 8),
            SyntheticTag::short(tags::PHOTOMETRIC, 1),
            SyntheticTag::short(tags::PREDICTOR, 2),
        ],
        |_, raw| tiff_deflate(&horizontal_difference(raw, width, 1)),
    );
    let slide = crate::Slide::open(file.path()).unwrap();
    let request = crate::RegionRequest::builder(0usize, 0usize, 0u32)
        .origin_px((250, 250))
        .size_px((10, 8))
        .build()
        .unwrap();

    let region = slide.read_region_rgba(&request).unwrap();

    let expected: Vec<u8> = strip_pattern_window(250, 250, 10, 8, 1)
        .into_iter()
        .flat_map(|gray| [gray, gray, gray, 255])
        .collect();
    assert_eq!(region.as_raw(), &expected);
}
