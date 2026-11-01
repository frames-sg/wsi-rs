use super::super::frame::{
    merge_missing_jpeg_tables, next_jpeg_segment, standalone_jpeg_frame_owned,
};
use crate::error::WsiError;
use jpeg_encoder::{ColorType, Encoder};

fn jpeg(progressive: bool) -> Vec<u8> {
    let pixels = (0..16 * 16 * 3)
        .map(|i| (i * 37 + i / 16) as u8)
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    let mut encoder = Encoder::new(&mut bytes, 90);
    encoder.set_progressive(progressive);
    encoder.encode(&pixels, 16, 16, ColorType::Rgb).unwrap();
    bytes
}

/// Move selected table segments before the first scan into TIFF JPEGTables.
fn abbreviated(source: &[u8], remove: impl Fn(u8, u8) -> bool) -> (Vec<u8>, Vec<u8>) {
    let mut tile = vec![0xFF, 0xD8];
    let mut tables = vec![0xFF, 0xD8];
    let mut offset = 2;
    while let Some(segment) = next_jpeg_segment(source, offset).unwrap() {
        if segment.marker == 0xDA {
            tile.extend_from_slice(&source[segment.start..]);
            break;
        }
        let bytes = &source[segment.start..segment.end];
        if matches!(segment.marker, 0xDB | 0xC4) && remove(segment.marker, segment.payload[0]) {
            tables.extend_from_slice(bytes);
        } else {
            tile.extend_from_slice(bytes);
        }
        offset = segment.end;
    }
    tables.extend_from_slice(&[0xFF, 0xD9]);
    (tile, tables)
}

fn decoded(bytes: &[u8]) -> Vec<u8> {
    jpeg_decoder::Decoder::new(bytes)
        .decode()
        .expect("standalone JPEG decodes")
}

fn rebuilt_frames(tile: &[u8], tables: Option<&[u8]>) -> [Vec<u8>; 2] {
    [
        merge_missing_jpeg_tables(tile.to_vec(), tables).unwrap(),
        standalone_jpeg_frame_owned(tile.to_vec(), tables)
            .unwrap()
            .0,
    ]
}

#[test]
fn partial_quantization_table_tile_merges_and_decodes() {
    let source = jpeg(false);
    let (tile, mut tables) = abbreviated(&source, |marker, id| marker == 0xDB && id == 1);
    assert!(jpeg_decoder::Decoder::new(tile.as_slice())
        .decode()
        .is_err());

    // A conflicting shared table 0 must not override the tile's own table 0.
    let mut conflicting_dqt = vec![0xFF, 0xDB, 0, 67, 0];
    conflicting_dqt.extend_from_slice(&[1; 64]);
    tables.splice(2..2, conflicting_dqt);
    let expected = decoded(&source);
    for frame in rebuilt_frames(&tile, Some(&tables)) {
        assert!(
            frame != tile,
            "referenced quantization table 1 must be merged"
        );
        assert_eq!(decoded(&frame), expected);
    }
}

#[test]
fn partial_huffman_table_tile_merges_and_decodes() {
    let source = jpeg(false);
    let expected = decoded(&source);
    for missing in [0x01, 0x10] {
        let (tile, tables) = abbreviated(&source, |marker, id| marker == 0xC4 && id == missing);
        assert!(jpeg_decoder::Decoder::new(tile.as_slice())
            .decode()
            .is_err());
        for frame in rebuilt_frames(&tile, Some(&tables)) {
            assert!(
                frame != tile,
                "referenced Huffman table {missing:#x} must be merged"
            );
            assert_eq!(decoded(&frame), expected);
        }
    }
}

#[test]
fn complete_tile_is_byte_identical() {
    let source = jpeg(false);
    let (_, tables) = abbreviated(&source, |_, _| true);
    for shared in [None, Some(tables.as_slice())] {
        for frame in rebuilt_frames(&source, shared) {
            assert_eq!(frame, source);
        }
    }
}

#[test]
fn tableless_tile_merges_as_before() {
    let source = jpeg(false);
    let (tile, tables) = abbreviated(&source, |_, _| true);
    let mut expected = tile[..2].to_vec();
    expected.extend_from_slice(&tables[2..tables.len() - 2]);
    expected.extend_from_slice(&tile[2..]);
    for frame in rebuilt_frames(&tile, Some(&tables)) {
        assert_eq!(frame, expected);
        assert_eq!(decoded(&frame), decoded(&source));
    }
}

#[test]
fn missing_referenced_table_without_jpeg_tables_errors() {
    let source = jpeg(false);
    for (marker, id) in [(0xDB, 1), (0xC4, 0x01), (0xC4, 0x10)] {
        let (tile, _) = abbreviated(&source, |m, t| m == marker && t == id);
        for result in [
            merge_missing_jpeg_tables(tile.clone(), None),
            standalone_jpeg_frame_owned(tile.clone(), None).map(|(frame, _)| frame),
        ] {
            assert!(matches!(
                result,
                Err(WsiError::Unsupported { reason }) if reason.contains("no JPEGTables")
            ));
        }
    }
}

#[test]
fn progressive_tile_only_requires_first_scan_huffman_tables() {
    let (mut source, later_tables) =
        abbreviated(&jpeg(true), |marker, id| marker == 0xC4 && id >> 4 == 1);
    let mut offset = 2;
    let first_scan = loop {
        let segment = next_jpeg_segment(&source, offset).unwrap().unwrap();
        if segment.marker == 0xDA {
            break segment;
        }
        offset = segment.end;
    };
    assert_eq!(&first_scan.payload[3..5], &[0, 0], "first scan is DC only");
    let next_scan = next_jpeg_segment(&source, first_scan.end)
        .unwrap()
        .unwrap()
        .start;
    source.splice(
        next_scan..next_scan,
        later_tables[2..later_tables.len() - 2].iter().copied(),
    );
    assert_eq!(
        merge_missing_jpeg_tables(source.clone(), None).unwrap(),
        source
    );
    let (tile, tables) = abbreviated(&source, |marker, id| marker == 0xC4 && id == 0);
    let frame = merge_missing_jpeg_tables(tile.clone(), Some(&tables)).unwrap();
    assert!(frame != tile, "first-scan DC table 0 must be merged");
    assert_eq!(decoded(&frame), decoded(&source));
}
