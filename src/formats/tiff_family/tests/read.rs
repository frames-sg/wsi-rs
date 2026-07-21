use super::super::*;
use super::fixtures::*;
use crate::core::types::ColorSpace;

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

    // Skip probe — call open() directly
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

    // Read tile (0, 0) — should succeed with JPEG-decoded data
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
