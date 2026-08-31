use super::*;
use crate::formats::tiff_family::container::TiffContainer;
use std::io::Write;
use tempfile::NamedTempFile;

/// Build a minimal classic TIFF file in memory with the given IFDs.
/// Each IFD is a list of (tag, type_id, count, value_bytes).
/// Supports only inline tags (value fits in 4 bytes) for simplicity.
#[allow(clippy::type_complexity)]
fn build_synthetic_tiff(ifds: &[Vec<(u16, u16, u32, [u8; 4])>]) -> NamedTempFile {
    let mut buf = Vec::new();

    // TIFF header: little-endian, classic TIFF
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes());

    let mut ifd_offsets = Vec::new();
    let mut next_ifd_patch_positions = Vec::new();

    for tags in ifds.iter() {
        let ifd_offset = buf.len() as u32;
        ifd_offsets.push(ifd_offset);

        let mut all_tags = tags.clone();
        all_tags.sort_by_key(|t| t.0);

        let entry_count = all_tags.len() as u16;
        buf.extend_from_slice(&entry_count.to_le_bytes());

        for (tag_id, type_id, count, value) in &all_tags {
            buf.extend_from_slice(&tag_id.to_le_bytes());
            buf.extend_from_slice(&type_id.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
            buf.extend_from_slice(value);
        }

        let next_pos = buf.len();
        buf.extend_from_slice(&0u32.to_le_bytes());
        next_ifd_patch_positions.push(next_pos);
    }

    // Patch first IFD offset.
    let offset_bytes = ifd_offsets[0].to_le_bytes();
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&offset_bytes);

    // Chain IFDs.
    for i in 0..ifd_offsets.len() - 1 {
        let patch_pos = next_ifd_patch_positions[i];
        let next_offset = ifd_offsets[i + 1];
        let bytes = next_offset.to_le_bytes();
        buf[patch_pos..patch_pos + 4].copy_from_slice(&bytes);
    }

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

/// Helper: create a LONG tag value (type_id=4, count=1).
fn long_tag(tag: u16, value: u32) -> (u16, u16, u32, [u8; 4]) {
    (tag, 4, 1, value.to_le_bytes())
}

/// Helper: create a SHORT tag value (type_id=3, count=1), stored in first 2 bytes.
fn short_tag(tag: u16, value: u16) -> (u16, u16, u32, [u8; 4]) {
    let mut val = [0u8; 4];
    val[0..2].copy_from_slice(&value.to_le_bytes());
    (tag, 3, 1, val)
}

fn clone_tempfile(src: &NamedTempFile) -> NamedTempFile {
    let bytes = std::fs::read(src.path()).unwrap();
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&bytes).unwrap();
    file.flush().unwrap();
    file
}

// ── Detection tests ──────────────────────────────────────────────

#[test]
fn detect_tiled_tiff() {
    let file = build_synthetic_tiff(&[vec![
        long_tag(tags::IMAGE_WIDTH, 1024),
        long_tag(tags::IMAGE_LENGTH, 768),
        long_tag(tags::TILE_WIDTH, 256),
        long_tag(tags::TILE_LENGTH, 256),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    assert!(interp.detect(&container));
}

#[test]
fn reject_non_tiled_tiff() {
    // No TILE_WIDTH tag -> not tiled.
    let file = build_synthetic_tiff(&[vec![
        long_tag(tags::IMAGE_WIDTH, 1024),
        long_tag(tags::IMAGE_LENGTH, 768),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    assert!(!interp.detect(&container));
}

#[test]
fn reject_ndpi() {
    // NDPI marker tag present -> NdpiInterpreter should handle it.
    let file = build_synthetic_tiff(&[vec![
        long_tag(tags::IMAGE_WIDTH, 1024),
        long_tag(tags::IMAGE_LENGTH, 768),
        long_tag(tags::TILE_WIDTH, 256),
        long_tag(tags::TILE_LENGTH, 256),
        long_tag(tags::NDPI_MARKER, 1),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    assert!(!interp.detect(&container));
}

// ── Interpret tests ──────────────────────────────────────────────

#[test]
fn interpret_single_level() {
    let file = build_synthetic_tiff(&[vec![
        long_tag(tags::IMAGE_WIDTH, 1024),
        long_tag(tags::IMAGE_LENGTH, 768),
        long_tag(tags::TILE_WIDTH, 256),
        long_tag(tags::TILE_LENGTH, 256),
        short_tag(tags::COMPRESSION, 7), // JPEG
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    let layout = interp.interpret(&container).unwrap();

    assert_eq!(layout.dataset.scenes.len(), 1);
    let series = &layout.dataset.scenes[0].series[0];
    assert_eq!(series.levels.len(), 1);
    assert_eq!(series.levels[0].dimensions, (1024, 768));
    assert!((series.levels[0].downsample - 1.0).abs() < 0.001);

    // tiles_across = ceil(1024/256) = 4, tiles_down = ceil(768/256) = 3
    match &series.levels[0].tile_layout {
        TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } => {
            assert_eq!(*tile_width, 256);
            assert_eq!(*tile_height, 256);
            assert_eq!(*tiles_across, 4);
            assert_eq!(*tiles_down, 3);
        }
        other => panic!("expected Regular tile layout, got: {:?}", other),
    }

    // Tile source should exist for level 0.
    let key = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    assert!(layout.tile_sources.contains_key(&key));
    match layout.tile_sources.get(&key).unwrap() {
        TileSource::TiledIfd { compression, .. } => {
            assert_eq!(*compression, Compression::Jpeg);
        }
        other => panic!("expected TiledIfd, got: {:?}", other),
    }

    // Vendor property.
    assert_eq!(layout.dataset.properties.vendor(), Some("generic-tiff"),);
    assert!(layout.dataset.properties.quickhash1().is_some());
}

#[test]
fn dataset_identity_is_path_independent_for_same_contents() {
    let file_a = build_synthetic_tiff(&[vec![
        long_tag(tags::IMAGE_WIDTH, 1024),
        long_tag(tags::IMAGE_LENGTH, 768),
        long_tag(tags::TILE_WIDTH, 256),
        long_tag(tags::TILE_LENGTH, 256),
        short_tag(tags::COMPRESSION, 7),
    ]]);
    let file_b = clone_tempfile(&file_a);

    let container_a = TiffContainer::open(file_a.path()).unwrap();
    let container_b = TiffContainer::open(file_b.path()).unwrap();
    let interp = GenericTiffInterpreter;
    let layout_a = interp.interpret(&container_a).unwrap();
    let layout_b = interp.interpret(&container_b).unwrap();

    assert_eq!(layout_a.dataset.id, layout_b.dataset.id);
    assert_eq!(
        layout_a.dataset.properties.quickhash1(),
        layout_b.dataset.properties.quickhash1()
    );
}

#[test]
fn interpret_multi_level_sorted() {
    // Two tiled IFDs: smaller first in file, larger second.
    // Interpreter should sort largest as level 0.
    let file = build_synthetic_tiff(&[
        vec![
            long_tag(tags::IMAGE_WIDTH, 512),
            long_tag(tags::IMAGE_LENGTH, 384),
            long_tag(tags::TILE_WIDTH, 256),
            long_tag(tags::TILE_LENGTH, 256),
        ],
        vec![
            long_tag(tags::IMAGE_WIDTH, 2048),
            long_tag(tags::IMAGE_LENGTH, 1536),
            long_tag(tags::TILE_WIDTH, 256),
            long_tag(tags::TILE_LENGTH, 256),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    let layout = interp.interpret(&container).unwrap();

    let series = &layout.dataset.scenes[0].series[0];
    assert_eq!(series.levels.len(), 2);

    // Level 0 = largest.
    assert_eq!(series.levels[0].dimensions, (2048, 1536));
    assert!((series.levels[0].downsample - 1.0).abs() < 0.001);

    // Level 1 = smaller.
    assert_eq!(series.levels[1].dimensions, (512, 384));
    assert!(series.levels[1].downsample > 1.0);
    // downsample ~ avg(2048/512, 1536/384) / 1 = avg(4.0, 4.0) = 4.0
    assert!((series.levels[1].downsample - 4.0).abs() < 0.01);
}

#[test]
fn interpret_stripped_as_associated() {
    // One tiled IFD + one stripped IFD.
    let file = build_synthetic_tiff(&[
        vec![
            long_tag(tags::IMAGE_WIDTH, 1024),
            long_tag(tags::IMAGE_LENGTH, 768),
            long_tag(tags::TILE_WIDTH, 256),
            long_tag(tags::TILE_LENGTH, 256),
        ],
        vec![
            long_tag(tags::IMAGE_WIDTH, 400),
            long_tag(tags::IMAGE_LENGTH, 300),
            short_tag(tags::COMPRESSION, 7),
            long_tag(tags::STRIP_OFFSETS, 100),
            long_tag(tags::STRIP_BYTE_COUNTS, 500),
            (tags::JPEG_TABLES, 7, 4, [0xFF, 0xD8, 0xFF, 0xD9]),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    let layout = interp.interpret(&container).unwrap();

    // Pyramid should have 1 level.
    assert_eq!(layout.dataset.scenes[0].series[0].levels.len(), 1);

    // Associated image should exist.
    assert!(layout.dataset.associated_images.contains_key("image_0"));
    let ai = &layout.dataset.associated_images["image_0"];
    assert_eq!(ai.dimensions, (400, 300));

    // Associated source should exist.
    assert!(layout.associated_sources.contains_key("image_0"));
    match layout.associated_sources.get("image_0").unwrap() {
        TileSource::Stripped {
            jpeg_tables,
            strip_offsets,
            strip_byte_counts,
            ..
        } => {
            assert_eq!(jpeg_tables.as_deref(), Some(&[0xFF, 0xD8, 0xFF, 0xD9][..]));
            assert_eq!(strip_offsets.as_slice(), &[100]);
            assert_eq!(strip_byte_counts.as_slice(), &[500]);
        }
        other => panic!("expected Stripped, got: {:?}", other),
    }
}

#[test]
fn interpret_ignores_associated_only_icc_for_main_series() {
    let file = build_synthetic_tiff(&[
        vec![
            long_tag(tags::IMAGE_WIDTH, 1024),
            long_tag(tags::IMAGE_LENGTH, 768),
            long_tag(tags::TILE_WIDTH, 256),
            long_tag(tags::TILE_LENGTH, 256),
        ],
        vec![
            long_tag(tags::IMAGE_WIDTH, 400),
            long_tag(tags::IMAGE_LENGTH, 300),
            long_tag(tags::STRIP_OFFSETS, 100),
            long_tag(tags::STRIP_BYTE_COUNTS, 500),
            (tags::ICC_PROFILE, 7, 4, [1, 2, 3, 4]),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    let layout = interp.interpret(&container).unwrap();

    assert!(layout.dataset.source_icc_profiles.is_empty());
    assert!(layout.dataset.icc_profiles.is_empty());
    assert!(layout.dataset.associated_images.contains_key("image_0"));
}

#[test]
fn interpret_no_tiled_ifds_returns_error() {
    // Only stripped IFDs.
    let file = build_synthetic_tiff(&[vec![
        long_tag(tags::IMAGE_WIDTH, 400),
        long_tag(tags::IMAGE_LENGTH, 300),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    let result = interp.interpret(&container);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("No tiled IFDs"),
        "expected 'No tiled IFDs', got: {}",
        msg,
    );
}

#[test]
fn interpret_axes_default() {
    let file = build_synthetic_tiff(&[vec![
        long_tag(tags::IMAGE_WIDTH, 512),
        long_tag(tags::IMAGE_LENGTH, 512),
        long_tag(tags::TILE_WIDTH, 256),
        long_tag(tags::TILE_LENGTH, 256),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    let layout = interp.interpret(&container).unwrap();

    let axes = layout.dataset.scenes[0].series[0].axes;
    assert_eq!(axes, AxesShape { z: 1, c: 1, t: 1 });
}

#[test]
fn interpret_tile_count_ceil() {
    // Width not evenly divisible by tile width.
    // 1000 / 256 = 3.90625 -> tiles_across = 4
    // 500 / 256 = 1.953125 -> tiles_down = 2
    let file = build_synthetic_tiff(&[vec![
        long_tag(tags::IMAGE_WIDTH, 1000),
        long_tag(tags::IMAGE_LENGTH, 500),
        long_tag(tags::TILE_WIDTH, 256),
        long_tag(tags::TILE_LENGTH, 256),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interp = GenericTiffInterpreter;
    let layout = interp.interpret(&container).unwrap();

    match &layout.dataset.scenes[0].series[0].levels[0].tile_layout {
        TileLayout::Regular {
            tiles_across,
            tiles_down,
            ..
        } => {
            assert_eq!(*tiles_across, 4);
            assert_eq!(*tiles_down, 2);
        }
        other => panic!("expected Regular, got: {:?}", other),
    }
}

#[test]
fn compression_mapping() {
    assert_eq!(compression_from_tag(1), Compression::None);
    assert_eq!(compression_from_tag(6), Compression::Jpeg);
    assert_eq!(compression_from_tag(7), Compression::Jpeg);
    assert_eq!(compression_from_tag(5), Compression::Lzw);
    assert_eq!(compression_from_tag(8), Compression::Deflate);
    assert_eq!(compression_from_tag(32946), Compression::Deflate);
    assert_eq!(compression_from_tag(50000), Compression::Zstd);
    assert_eq!(compression_from_tag(33003), Compression::Jp2kYcbcr);
    assert_eq!(compression_from_tag(33005), Compression::Jp2kYcbcr);
    assert_eq!(compression_from_tag(33004), Compression::Jp2kRgb);
    assert_eq!(compression_from_tag(999), Compression::Other(999));
}
