use super::super::frame::{
    disable_jpeg_restart_interval, jpeg_header_reaches_scan, jpeg_segment_color_hint,
    merge_missing_jpeg_tables, parse_baseline_jpeg_frame_info, parse_jpeg_frame_info,
    patch_jpeg_sof0_dimensions, standalone_jpeg_frame_owned, JpegBitstreamColorHint,
};
use crate::error::WsiError;
use crate::test_support::jpeg_segments_through_first_scan;
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
    for segment in jpeg_segments_through_first_scan(source) {
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
    let first_scan = jpeg_segments_through_first_scan(&source)
        .pop()
        .filter(|segment| segment.marker == 0xDA)
        .expect("the fixture has a scan");
    assert_eq!(&first_scan.payload[3..5], &[0, 0], "first scan is DC only");
    let next_scan = j2k_jpeg::find_scan_ranges(&source)
        .unwrap()
        .entropy_range
        .end;
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

/// The stored bytes of the `marker` segments of `stream`, in order.
fn segments_of(stream: &[u8], marker: u8) -> Vec<u8> {
    jpeg_segments_through_first_scan(stream)
        .into_iter()
        .filter(|segment| segment.marker == marker)
        .flat_map(|segment| stream[segment.start..segment.end].to_vec())
        .collect()
}

#[test]
fn only_the_kinds_of_shared_tables_a_tile_lacks_are_merged() {
    let source = jpeg(false);
    // Shared tables of both kinds, as TIFF writers store them.
    let (_, tables) = abbreviated(&source, |_, _| true);
    let (dqt, dht) = (segments_of(&tables, 0xDB), segments_of(&tables, 0xC4));
    assert!(!dqt.is_empty() && !dht.is_empty());

    for (missing, shared) in [(0xDB, &dqt), (0xC4, &dht)] {
        // The tile defines the other kind itself: only `missing` is merged,
        // right after SOI, and the tile's own bytes follow unchanged.
        let (tile, _) = abbreviated(&source, |marker, _| marker == missing);
        let mut expected = tile[..2].to_vec();
        expected.extend_from_slice(shared);
        expected.extend_from_slice(&tile[2..]);
        for frame in rebuilt_frames(&tile, Some(&tables)) {
            assert_eq!(frame, expected, "missing FF{missing:02X}");
            assert_eq!(decoded(&frame), decoded(&source));
        }
    }

    // Segments of other kinds in the shared stream are never merged.
    let mut with_comment = tables[..2].to_vec();
    with_comment.extend_from_slice(&[0xFF, 0xFE, 0, 4, b'h', b'i']);
    with_comment.extend_from_slice(&tables[2..]);
    let (tile, _) = abbreviated(&source, |_, _| true);
    let mut expected = tile[..2].to_vec();
    expected.extend_from_slice(&tables[2..tables.len() - 2]);
    expected.extend_from_slice(&tile[2..]);
    for frame in rebuilt_frames(&tile, Some(&with_comment)) {
        assert_eq!(frame, expected);
    }
}

#[test]
fn shared_tables_that_do_not_complete_the_tile_are_an_error() {
    let source = jpeg(false);
    let (tile, tables) = abbreviated(&source, |_, _| true);
    // Quantization tables only: the tile's Huffman tables stay undefined.
    let mut dqt_only = tables[..2].to_vec();
    dqt_only.extend_from_slice(&segments_of(&tables, 0xDB));
    dqt_only.extend_from_slice(&[0xFF, 0xD9]);
    for result in [
        merge_missing_jpeg_tables(tile.clone(), Some(&dqt_only)),
        standalone_jpeg_frame_owned(tile.clone(), Some(&dqt_only)).map(|(frame, _)| frame),
    ] {
        assert!(
            matches!(
                &result,
                Err(WsiError::Unsupported { reason })
                    if reason.contains("could not rebuild required DQT/DHT table segments")
            ),
            "{result:?}"
        );
    }
    // A stream that is not a JPEG table stream offers nothing.
    assert!(merge_missing_jpeg_tables(tile.clone(), Some(&[0, 1, 2, 3])).is_err());
    // Tiles must be SOI/EOI-delimited before anything is merged.
    for bad in [&tile[2..], &tile[..tile.len() - 2]] {
        assert!(matches!(
            merge_missing_jpeg_tables(bad.to_vec(), Some(&tables)),
            Err(WsiError::Unsupported { .. })
        ));
    }
}

#[test]
fn tile_j2k_cannot_assemble_is_unsupported_not_handed_on() {
    // An arithmetic-coded frame: j2k-jpeg does not judge its tables.
    let mut arithmetic = jpeg(false);
    let sof = jpeg_segments_through_first_scan(&arithmetic)
        .into_iter()
        .find(|segment| segment.marker == 0xC0)
        .unwrap()
        .start;
    arithmetic[sof + 1] = 0xC9;
    let (_, tables) = abbreviated(&jpeg(false), |_, _| true);
    for shared in [None, Some(tables.as_slice())] {
        let result = merge_missing_jpeg_tables(arithmetic.clone(), shared);
        assert!(
            matches!(
                &result,
                Err(WsiError::Unsupported { reason })
                    if reason.contains("could not prepare the tile")
            ),
            "{result:?}"
        );
    }

    // Shared tables with a malformed definition fail the same way.
    let (tile, tables) = abbreviated(&jpeg(false), |_, _| true);
    let mut malformed = tables.clone();
    let dht = malformed
        .windows(2)
        .position(|m| m == [0xFF, 0xC4])
        .unwrap();
    malformed[dht + 4] = 0x40;
    let result = merge_missing_jpeg_tables(tile, Some(&malformed));
    assert!(
        matches!(&result, Err(WsiError::Unsupported { reason }) if reason.contains("could not")),
        "{result:?}"
    );
}

/// Zero the size of the first SOF0 of `jpeg`.
fn with_zero_sof_size(jpeg: &[u8]) -> Vec<u8> {
    let sof = jpeg_segments_through_first_scan(jpeg)
        .into_iter()
        .find(|segment| segment.marker == 0xC0)
        .unwrap()
        .start;
    let mut out = jpeg.to_vec();
    out[sof + 5..sof + 9].fill(0);
    out
}

#[test]
fn complete_tile_stating_a_zero_size_is_returned_as_stored() {
    let zero = with_zero_sof_size(&jpeg(false));
    assert_eq!(merge_missing_jpeg_tables(zero.clone(), None).unwrap(), zero);
    // Raw passthrough of a baseline frame still needs its size.
    assert!(matches!(
        standalone_jpeg_frame_owned(zero, None),
        Err(WsiError::Unsupported { reason }) if reason.contains("nonzero SOF dimensions")
    ));
}

#[test]
fn bytes_outside_marker_segments_are_skipped_as_libjpeg_skips_them() {
    let source = jpeg(false);
    let info = parse_baseline_jpeg_frame_info(&source).unwrap();
    let sof = jpeg_segments_through_first_scan(&source)
        .into_iter()
        .find(|segment| segment.marker == 0xC0)
        .unwrap()
        .start;

    // Stray bytes after SOI and in front of SOF, and a stuffed FF 00.
    let mut stray = source.clone();
    stray.splice(sof..sof, [0x12, 0x34, 0x56]);
    stray.splice(2..2, [0x00, 0xFF, 0x00]);
    assert!(j2k_jpeg::Decoder::inspect(&stray).is_err());
    assert!(jpeg_decoder::Decoder::new(stray.as_slice())
        .decode()
        .is_ok());

    for frame in [
        parse_baseline_jpeg_frame_info(&stray).unwrap(),
        parse_jpeg_frame_info(&stray).unwrap(),
    ] {
        assert_eq!((frame.width, frame.height), (info.width, info.height));
        assert_eq!(frame.samples_per_pixel, 3);
    }
    assert_eq!(
        jpeg_segment_color_hint(&stray),
        jpeg_segment_color_hint(&source)
    );
    assert!(jpeg_header_reaches_scan(&stray));
    assert!(!jpeg_header_reaches_scan(&stray[..sof]));
    // A complete tile is handed on exactly as stored.
    for frame in rebuilt_frames(&stray, None) {
        assert_eq!(frame, stray);
    }

    // An abbreviated one gets its tables; the stray bytes are not kept.
    let (tile, tables) = abbreviated(&source, |_, _| true);
    let mut stray_tile = tile.clone();
    stray_tile.insert(2, 0x00);
    let merged = merge_missing_jpeg_tables(stray_tile, Some(&tables)).unwrap();
    assert_eq!(
        merged,
        merge_missing_jpeg_tables(tile, Some(&tables)).unwrap()
    );

    // Stray bytes are skipped a bounded number of times.
    let mut riddled = source.clone();
    for _ in 0..17 {
        riddled.splice(2..2, [0xFF, 0xFE, 0, 2, 0x00]);
    }
    assert!(parse_baseline_jpeg_frame_info(&riddled).is_err());
    riddled.drain(2..7);
    assert!(parse_baseline_jpeg_frame_info(&riddled).is_ok());
}

#[test]
fn baseline_frame_is_the_last_sof0_and_other_processes_are_refused() {
    let source = jpeg(false);
    let sof = jpeg_segments_through_first_scan(&source)
        .into_iter()
        .find(|segment| segment.marker == 0xC0)
        .unwrap();
    let mut early = source[sof.start..sof.end].to_vec();
    early[5..9].copy_from_slice(&[0, 32, 0, 48]);
    let mut double_sof = source.clone();
    double_sof.splice(2..2, early);

    let last = parse_baseline_jpeg_frame_info(&double_sof).unwrap();
    assert_eq!((last.width, last.height), (16, 16));
    // The frame handed on whole is the first one of any coding process.
    let first = parse_jpeg_frame_info(&double_sof).unwrap();
    assert_eq!((first.width, first.height), (48, 32));

    let progressive = jpeg(true);
    assert!(matches!(
        parse_baseline_jpeg_frame_info(&progressive),
        Err(WsiError::Unsupported { reason }) if reason.contains("Baseline JPEG SOF0")
    ));
    let frame = parse_jpeg_frame_info(&progressive).unwrap();
    assert_eq!(
        (frame.width, frame.height, frame.bits_allocated),
        (16, 16, 8)
    );
    assert!(parse_jpeg_frame_info(&with_zero_sof_size(&source)).is_none());
    assert!(parse_jpeg_frame_info(&source[..sof.start]).is_none());
    assert!(matches!(
        parse_baseline_jpeg_frame_info(&[0xFF, 0xD8, 0xFF, 0xD9]),
        Err(WsiError::Unsupported { reason }) if reason.contains("could not find")
    ));
}

#[test]
fn frame_sizes_past_the_decoder_limit_are_still_described_for_passthrough() {
    // JPEG allows 65535 pixels a side; j2k-jpeg decodes up to 65500. Raw
    // passthrough hands such a frame on, so it must still be described.
    let mut wide = jpeg(false);
    let sof = jpeg_segments_through_first_scan(&wide)
        .into_iter()
        .find(|segment| segment.marker == 0xC0)
        .unwrap()
        .start;
    wide[sof + 7..sof + 9].copy_from_slice(&65_535u16.to_be_bytes());
    assert!(j2k_jpeg::Decoder::inspect(&wide).is_err());
    let frame = parse_baseline_jpeg_frame_info(&wide).unwrap();
    assert_eq!((frame.width, frame.height), (65_535, 16));

    for (byte, value, expected) in [
        (4, 12, "8-bit JPEG, got 12-bit"),
        (9, 2, "1 or 3 components, got 2"),
        (9, 4, "component table is truncated"),
    ] {
        let mut patched = jpeg(false);
        patched[sof + byte] = value;
        assert!(matches!(
            parse_baseline_jpeg_frame_info(&patched),
            Err(WsiError::Unsupported { reason }) if reason.contains(expected)
        ));
    }
    let mut truncated = jpeg(false)[..sof + 8].to_vec();
    truncated[sof + 3] = 6;
    assert!(parse_baseline_jpeg_frame_info(&truncated).is_err());
}

/// A header through SOS with a DRI of `interval` MCUs in front of the scan.
fn header_with_restart_interval(interval: u16) -> Vec<u8> {
    let source = jpeg(false);
    let sos = jpeg_segments_through_first_scan(&source).pop().unwrap();
    let mut header = source[..sos.end].to_vec();
    let mut dri = vec![0xFF, 0xDD, 0, 4];
    dri.extend_from_slice(&interval.to_be_bytes());
    header.splice(sos.start..sos.start, dri);
    header
}

#[test]
fn restart_interval_is_zeroed_in_the_header_only() {
    let mut header = header_with_restart_interval(8);
    // An APP payload that holds DRI-like bytes must not be touched.
    let decoy = [0xFF, 0xE1, 0, 8, 0xFF, 0xDD, 0, 4, 0, 9];
    header.splice(2..2, decoy);
    let dri = header
        .windows(4)
        .rposition(|w| w == [0xFF, 0xDD, 0, 4])
        .unwrap();
    assert_eq!(
        j2k_jpeg::Decoder::inspect(&header)
            .unwrap()
            .restart_interval,
        Some(8)
    );

    let mut disabled = header.clone();
    disable_jpeg_restart_interval(&mut disabled);
    assert_eq!(&disabled[dri + 4..dri + 6], &[0, 0]);
    assert_eq!(&disabled[2..12], &decoy);
    let mut expected = header.clone();
    expected[dri + 4..dri + 6].fill(0);
    assert_eq!(disabled, expected);
    assert_eq!(
        j2k_jpeg::Decoder::inspect(&disabled)
            .unwrap()
            .restart_interval,
        None
    );

    // Headers without a restart interval, or with entropy-coded bytes that
    // look like one after the scan header, are left alone.
    let mut plain = jpeg(false);
    plain.splice(plain.len() - 2..plain.len() - 2, [0xFF, 0xDD, 0, 4, 0, 7]);
    let before = plain.clone();
    disable_jpeg_restart_interval(&mut plain);
    assert_eq!(plain, before);
    let mut not_jpeg = vec![0u8; 16];
    disable_jpeg_restart_interval(&mut not_jpeg);
    assert_eq!(not_jpeg, [0u8; 16]);
}

#[test]
fn sof0_size_is_patched_in_place_through_the_located_frame() {
    let source = jpeg(false);
    let sof = jpeg_segments_through_first_scan(&source)
        .into_iter()
        .find(|segment| segment.marker == 0xC0)
        .unwrap()
        .start;
    // SOF0-like bytes inside an APP payload in front of the real frame.
    let mut patched = source.clone();
    let decoy = [0xFF, 0xE1, 0, 13, 0xFF, 0xC0, 0, 11, 8, 0, 1, 0, 1, 1, 0];
    patched.splice(2..2, decoy);
    let sof = sof + decoy.len();
    patch_jpeg_sof0_dimensions(&mut patched, 640, 480).unwrap();
    assert_eq!(&patched[2..2 + decoy.len()], &decoy);
    assert_eq!(&patched[sof + 5..sof + 9], &[1, 0xE0, 2, 0x80]);
    let frame = parse_baseline_jpeg_frame_info(&patched).unwrap();
    assert_eq!((frame.width, frame.height), (640, 480));

    for (width, height) in [(0, 8), (8, 0), (65_536, 8), (8, 65_536)] {
        let mut unchanged = source.clone();
        assert!(matches!(
            patch_jpeg_sof0_dimensions(&mut unchanged, width, height),
            Err(WsiError::Unsupported { reason }) if reason.contains("u16 SOF dimensions")
        ));
        assert_eq!(unchanged, source);
    }
    for (mut input, expected) in [
        (jpeg(true), "only supports Baseline JPEG SOF0"),
        (
            vec![0xFF, 0xD8, 0xFF, 0xD9],
            "could not find a Baseline JPEG SOF0",
        ),
        (
            vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 5, 8, 0, 1],
            "SOF0 segment is truncated",
        ),
    ] {
        assert!(matches!(
            patch_jpeg_sof0_dimensions(&mut input, 8, 8),
            Err(WsiError::Unsupported { reason }) if reason.contains(expected)
        ));
    }
}

#[test]
fn color_hint_reads_the_first_adobe_or_frame_segment() {
    let source = jpeg(false);
    assert_eq!(
        jpeg_segment_color_hint(&source),
        JpegBitstreamColorHint::RgbComponentIds012
    );
    for (transform, expected) in [
        (0, JpegBitstreamColorHint::Rgb),
        (1, JpegBitstreamColorHint::YCbCr),
        (2, JpegBitstreamColorHint::Unknown),
    ] {
        let mut adobe = source.clone();
        adobe.splice(
            2..2,
            [
                0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, transform,
            ],
        );
        assert_eq!(jpeg_segment_color_hint(&adobe), expected);
    }
    // Shared tables carry no frame, an empty or truncated stream no hint.
    let (_, tables) = abbreviated(&source, |_, _| true);
    for stream in [tables.as_slice(), &[], &source[..3], &[0, 1, 2, 3]] {
        assert_eq!(
            jpeg_segment_color_hint(stream),
            JpegBitstreamColorHint::Unknown
        );
    }
}
