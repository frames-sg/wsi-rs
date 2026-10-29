use super::*;
use crate::formats::tiff_family::container::TiffContainer;
use crate::formats::tiff_family::test_support::{build_bigtiff, build_tiff, SyntheticTag};
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
fn interpret_sorts_levels_whose_area_overflows_u64() {
    // BigTIFF LONG8 sizes: the 2^32 x 2^32 level's area does not fit in u64.
    let level = |side: u64| {
        vec![
            SyntheticTag::long8(tags::IMAGE_WIDTH, side),
            SyntheticTag::long8(tags::IMAGE_LENGTH, side),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
        ]
    };
    let file = build_bigtiff(&[level(1 << 31), level(1 << 32)]);

    let container = TiffContainer::open(file.path()).unwrap();
    let layout = GenericTiffInterpreter.interpret(&container).unwrap();

    let levels = &layout.dataset.scenes[0].series[0].levels;
    assert_eq!(levels[0].dimensions, (1 << 32, 1 << 32));
    assert_eq!(levels[1].dimensions, (1 << 31, 1 << 31));
    assert!((levels[1].downsample - 2.0).abs() < 0.001);
}

#[test]
fn jpeg_tables_are_read_per_level_with_level0_fallback() {
    let level0_tables = [0xFF, 0xD8, 0x00, 0x00];
    let level1_tables = [0xFF, 0xD8, 0x11, 0x11];
    let level = |side: u32| {
        vec![
            long_tag(tags::IMAGE_WIDTH, side),
            long_tag(tags::IMAGE_LENGTH, side),
            long_tag(tags::TILE_WIDTH, 256),
            long_tag(tags::TILE_LENGTH, 256),
            short_tag(tags::COMPRESSION, 7),
        ]
    };
    let with_tables = |mut ifd: Vec<_>, tables: [u8; 4]| {
        ifd.push((tags::JPEG_TABLES, 7, 4, tables));
        ifd
    };
    let file = build_synthetic_tiff(&[
        with_tables(level(2048), level0_tables),
        with_tables(level(1024), level1_tables),
        level(512),
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let layout = GenericTiffInterpreter.interpret(&container).unwrap();

    for (level, expected) in [(0, level0_tables), (1, level1_tables), (2, level0_tables)] {
        let key = TileSourceKey {
            scene: 0,
            series: 0,
            level,
            z: 0,
            c: 0,
            t: 0,
        };
        match &layout.tile_sources[&key] {
            TileSource::TiledIfd { jpeg_tables, .. } => {
                assert_eq!(jpeg_tables.as_deref(), Some(&expected[..]), "level {level}");
            }
            other => panic!("expected TiledIfd, got: {:?}", other),
        }
    }
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

// ── Strip-only detection ─────────────────────────────────────────

/// Tags of a strip-only image. The strip table points at nothing; detection
/// and layout never read strip data.
#[derive(Clone, Copy)]
struct StripImage {
    width: u32,
    height: u32,
    rows_per_strip: Option<u32>,
    samples: u16,
    bits: u16,
    compression: u16,
    photometric: u16,
    predictor: u16,
    planar: u16,
    orientation: u16,
}

impl StripImage {
    const RGB: Self = Self {
        width: 300,
        height: 200,
        rows_per_strip: Some(16),
        samples: 3,
        bits: 8,
        compression: 1,
        photometric: 2,
        predictor: 1,
        planar: 1,
        orientation: 1,
    };
    const GRAY: Self = Self {
        samples: 1,
        photometric: 1,
        ..Self::RGB
    };

    fn size(self, width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            ..self
        }
    }
    fn rows_per_strip(self, rows_per_strip: Option<u32>) -> Self {
        Self {
            rows_per_strip,
            ..self
        }
    }
    fn samples(self, samples: u16) -> Self {
        Self { samples, ..self }
    }
    fn bits(self, bits: u16) -> Self {
        Self { bits, ..self }
    }
    fn compression(self, compression: u16) -> Self {
        Self {
            compression,
            ..self
        }
    }
    fn photometric(self, photometric: u16) -> Self {
        Self {
            photometric,
            ..self
        }
    }
    fn predictor(self, predictor: u16) -> Self {
        Self { predictor, ..self }
    }
    fn planar(self, planar: u16) -> Self {
        Self { planar, ..self }
    }
    fn orientation(self, orientation: u16) -> Self {
        Self {
            orientation,
            ..self
        }
    }

    fn expected_strips(self) -> usize {
        let rows = self.rows_per_strip.unwrap_or(self.height).min(self.height);
        let planes = if self.planar == 2 && self.samples == 3 {
            3
        } else {
            1
        };
        self.height.div_ceil(rows.max(1)) as usize * planes
    }

    fn tags(self) -> Vec<SyntheticTag> {
        self.tags_with_strips(self.expected_strips())
    }

    fn tags_with_strips(self, strips: usize) -> Vec<SyntheticTag> {
        let mut tags = vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, self.width),
            SyntheticTag::long(tags::IMAGE_LENGTH, self.height),
            SyntheticTag::long_array(
                tags::BITS_PER_SAMPLE,
                &vec![u32::from(self.bits); usize::from(self.samples)],
            ),
            SyntheticTag::short(tags::COMPRESSION, self.compression),
            SyntheticTag::short(tags::PHOTOMETRIC, self.photometric),
            SyntheticTag::short(tags::ORIENTATION, self.orientation),
            SyntheticTag::short(tags::SAMPLES_PER_PIXEL, self.samples),
            SyntheticTag::short(tags::PLANAR_CONFIGURATION, self.planar),
            SyntheticTag::short(tags::PREDICTOR, self.predictor),
            SyntheticTag::long_array(tags::STRIP_OFFSETS, &vec![8; strips]),
            SyntheticTag::long_array(tags::STRIP_BYTE_COUNTS, &vec![1; strips]),
        ];
        if let Some(rows) = self.rows_per_strip {
            tags.push(SyntheticTag::long(tags::ROWS_PER_STRIP, rows));
        }
        tags
    }
}

fn detects(ifds: &[Vec<SyntheticTag>]) -> bool {
    let file = build_tiff(ifds);
    GenericTiffInterpreter.detect(&TiffContainer::open(file.path()).unwrap())
}

#[test]
fn detects_strip_only_images_the_stripped_reader_decodes() {
    let rgb = StripImage::RGB;
    let gray = StripImage::GRAY;
    let cases = [
        ("uncompressed RGB", rgb),
        ("uncompressed planar RGB", rgb.planar(2)),
        ("LZW RGB with predictor", rgb.compression(5).predictor(2)),
        ("Deflate grayscale", gray.compression(8)),
        ("Adobe Deflate grayscale", gray.compression(32946)),
        ("Zstd RGB", rgb.compression(50000)),
        ("MinIsWhite LZW", gray.compression(5).photometric(0)),
        ("JPEG YCbCr", rgb.compression(7).photometric(6)),
        ("JPEG RGB", rgb.compression(7)),
        ("JPEG grayscale", gray.compression(7)),
        ("planar tag on one sample", gray.compression(5).planar(2)),
        // Uncompressed tiles read only their own rows, so one strip may hold
        // more than the decoded-image limit.
        (
            "uncompressed single strip over the decoded-image limit",
            rgb.size(40_000, 40_000).rows_per_strip(None),
        ),
        (
            "RowsPerStrip beyond the image height",
            rgb.compression(5).rows_per_strip(Some(u32::MAX)),
        ),
    ];
    for (name, image) in cases {
        assert!(detects(&[image.tags()]), "{name} should be detected");
    }
}

#[test]
fn declines_strip_only_images_the_stripped_reader_cannot_decode() {
    let rgb = StripImage::RGB;
    let gray = StripImage::GRAY;
    let cases = [
        ("16-bit grayscale", gray.bits(16)),
        ("palette", gray.photometric(3)),
        ("RGBA", rgb.samples(4)),
        ("CMYK", rgb.samples(4).photometric(5)),
        ("uncompressed YCbCr", rgb.photometric(6)),
        ("MinIsWhite JPEG", gray.compression(7).photometric(0)),
        ("predictor without compression", rgb.predictor(2)),
        ("floating-point predictor", rgb.compression(5).predictor(3)),
        ("old-style JPEG", rgb.compression(6).photometric(6)),
        ("unknown compression", rgb.compression(34712)),
        ("planar LZW", rgb.compression(5).planar(2)),
        ("rotated", rgb.orientation(3)),
        ("zero RowsPerStrip", rgb.rows_per_strip(Some(0))),
        (
            "JPEG wider than one frame",
            rgb.compression(7).photometric(6).size(70_000, 200),
        ),
        (
            "compressed strip over the decoded-image limit",
            rgb.compression(5).size(40_000, 40_000).rows_per_strip(None),
        ),
    ];
    for (name, image) in cases {
        assert!(!detects(&[image.tags()]), "{name} should be declined");
    }

    let short_table = rgb.tags_with_strips(rgb.expected_strips() - 1);
    assert!(
        !detects(&[short_table]),
        "short strip table should be declined"
    );

    let mut text_rows = rgb.rows_per_strip(None).tags();
    text_rows.push(SyntheticTag::ascii(tags::ROWS_PER_STRIP, "16"));
    assert!(
        !detects(&[text_rows]),
        "unreadable RowsPerStrip should be declined"
    );
}

#[test]
fn strip_only_tiff_opens_its_largest_image_as_the_level() {
    let thumbnail = StripImage::RGB.size(64, 48);
    let main = StripImage::RGB.compression(5);
    let same_size_page = StripImage::RGB.compression(8);
    let file = build_tiff(&[thumbnail.tags(), main.tags(), same_size_page.tags()]);
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(GenericTiffInterpreter.detect(&container));

    let layout = GenericTiffInterpreter.interpret(&container).unwrap();

    let levels = &layout.dataset.scenes[0].series[0].levels;
    assert_eq!(levels.len(), 1);
    assert_eq!(levels[0].dimensions, (300, 200));
    match &layout.tile_sources[&TileSourceKey::plane0(0)] {
        TileSource::StrippedLevel {
            ifd_id,
            compression,
            ..
        } => {
            // The first of the two equally large images wins.
            assert_eq!(*ifd_id, container.top_ifds()[1]);
            assert_eq!(*compression, Compression::Lzw);
        }
        other => panic!("expected StrippedLevel, got: {other:?}"),
    }
    let mut associated: Vec<_> = layout.dataset.associated_images.keys().collect();
    associated.sort();
    assert_eq!(associated, ["image_0", "image_2"]);
}

#[test]
fn strip_only_tiff_with_unsupported_largest_image_is_declined() {
    // Opening the supported thumbnail would pass it off as the slide.
    let thumbnail = StripImage::RGB.size(64, 48);
    let main = StripImage::GRAY.bits(16);
    let file = build_tiff(&[thumbnail.tags(), main.tags()]);
    let container = TiffContainer::open(file.path()).unwrap();

    assert!(!GenericTiffInterpreter.detect(&container));
    let err = GenericTiffInterpreter.interpret(&container).unwrap_err();
    assert!(err.to_string().contains("No tiled IFDs"), "{err}");
}

#[test]
fn stripped_level_records_clamped_rows_per_strip_and_jpeg_tables() {
    let tables = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x02, 0xFF, 0xD9];
    let mut jpeg = StripImage::RGB
        .compression(7)
        .photometric(6)
        .rows_per_strip(Some(u32::MAX))
        .tags();
    jpeg.push(SyntheticTag::bytes(tags::JPEG_TABLES, tables.clone()));
    let without_rows = StripImage::GRAY.rows_per_strip(None).tags();

    for (ifd, expected_tables) in [(jpeg, Some(tables)), (without_rows, None)] {
        let file = build_tiff(&[ifd]);
        let container = TiffContainer::open(file.path()).unwrap();
        let layout = GenericTiffInterpreter.interpret(&container).unwrap();
        match &layout.tile_sources[&TileSourceKey::plane0(0)] {
            TileSource::StrippedLevel {
                rows_per_strip,
                jpeg_tables,
                ..
            } => {
                assert_eq!(*rows_per_strip, 200);
                assert_eq!(jpeg_tables, &expected_tables);
            }
            other => panic!("expected StrippedLevel, got: {other:?}"),
        }
    }
}

#[test]
fn strip_only_olympus_vsi_is_declined() {
    // A VSI file reaches the TIFF backend only without its companion folder;
    // its strip-based overview is not the slide.
    let source = build_tiff(&[StripImage::RGB.tags()]);
    let dir = tempfile::tempdir().unwrap();
    for (name, detected) in [
        ("slide.tif", true),
        ("slide.vsi", false),
        ("slide.VSI", false),
    ] {
        let path = dir.path().join(name);
        std::fs::copy(source.path(), &path).unwrap();
        let container = TiffContainer::open(&path).unwrap();
        assert_eq!(
            GenericTiffInterpreter.detect(&container),
            detected,
            "{name}"
        );
    }
}
