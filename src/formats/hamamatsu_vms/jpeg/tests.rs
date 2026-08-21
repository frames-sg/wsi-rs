use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
#[cfg(unix)]
use std::{os::fd::OwnedFd, os::unix::net::UnixStream};

use super::*;
use crate::formats::hamamatsu_vms::tests::fixtures::{write_jpeg_header, write_restart_jpeg};

fn sof_payload(width: u16, height: u16, precision: u8, sampling: u8) -> Vec<u8> {
    let mut payload = vec![precision];
    payload.extend_from_slice(&height.to_be_bytes());
    payload.extend_from_slice(&width.to_be_bytes());
    payload.extend_from_slice(&[3, 1, sampling, 0, 2, sampling, 0, 3, sampling, 0]);
    payload
}

fn parse_header_segments(segments: &[(u8, &[u8])]) -> Result<VmsJpegHeader, WsiError> {
    let temp = tempfile::tempdir().expect("temporary JPEG header directory");
    let path = temp.path().join("header.jpg");
    write_jpeg_header(&path, segments);
    read_vms_jpeg_header(&path)
}

fn expect_error<T>(result: Result<T, WsiError>, context: &str) -> WsiError {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

fn scan_one_restart_offset(
    file: &mut File,
    file_len: u64,
    path: &Path,
) -> Result<Option<u64>, WsiError> {
    let mut offset = [None];
    find_restart_offsets(file, file_len, path, &mut offset)?;
    Ok(offset[0])
}

fn poison<T>(mutex: &Mutex<T>) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = mutex.lock().expect("unpoisoned fixture mutex");
        panic!("poison synthetic mutex");
    }));
    assert!(mutex.is_poisoned());
}

#[test]
fn entropy_budget_includes_the_reconstructed_header() {
    assert_eq!(
        checked_vms_entropy_len(MAX_COMPRESSED_INPUT_BYTES - 4, 4).unwrap(),
        (MAX_COMPRESSED_INPUT_BYTES - 4) as usize
    );
    assert!(matches!(
        checked_vms_entropy_len(MAX_COMPRESSED_INPUT_BYTES - 3, 4),
        Err(WsiError::ResourceLimit { .. })
    ));
    assert!(matches!(
        checked_vms_entropy_len(u64::MAX, 1),
        Err(WsiError::Jpeg(message)) if message.contains("length overflow")
    ));
}

#[test]
fn tile_index_budget_counts_both_dense_offset_vectors() {
    let path = Path::new("huge-vms.jpg");
    let entries_at_limit =
        MAX_VMS_TILE_INDEX_BYTES / (2 * std::mem::size_of::<Option<u64>>() as u64);
    assert_eq!(
        checked_vms_tile_count(path, entries_at_limit as u32, 1).unwrap(),
        entries_at_limit as usize
    );
    assert!(matches!(
        checked_vms_tile_count(path, entries_at_limit as u32 + 1, 1),
        Err(WsiError::ResourceLimit { .. })
    ));
    assert!(matches!(
        checked_vms_tile_count(path, u32::MAX, u32::MAX),
        Err(WsiError::InvalidSlide { message, .. }) if message.contains("index size overflow")
    ));
}

#[test]
fn jpeg_header_rejects_missing_markers_and_malformed_segments() {
    let temp = tempfile::tempdir().expect("temporary malformed JPEG directory");
    let path = temp.path().join("bad.jpg");

    for (bytes, expected) in [
        (vec![0, 0], "missing SOI marker"),
        (vec![0xFF, 0xD8, 0xFF, 0xD9], "ended before SOS"),
        (
            vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 1],
            "invalid VMS JPEG segment length",
        ),
        (vec![0xFF, 0xD8, 0xFF], "I/O error"),
    ] {
        fs::write(&path, bytes).expect("write malformed JPEG");
        let error = expect_error(read_vms_jpeg_header(&path), expected);
        assert!(error.to_string().contains(expected), "unexpected: {error}");
    }

    let missing = temp.path().join("missing.jpg");
    assert!(matches!(
        read_vms_jpeg_header(&missing),
        Err(WsiError::IoWithPath { path, .. }) if path == missing
    ));
}

#[test]
fn jpeg_header_rejects_invalid_sof_and_dri_semantics() {
    let short_sof = [8u8];
    assert!(expect_error(
        parse_header_segments(&[(0xC0, &short_sof)]),
        "short SOF must fail",
    )
    .to_string()
    .contains("truncated VMS JPEG SOF"));

    let precision = sof_payload(128, 16, 12, 0x11);
    assert!(expect_error(
        parse_header_segments(&[(0xC0, &precision)]),
        "unsupported precision must fail",
    )
    .to_string()
    .contains("unsupported VMS JPEG precision"));

    let zero_components = [8, 0, 16, 0, 128, 0];
    assert!(expect_error(
        parse_header_segments(&[(0xC0, &zero_components)]),
        "zero components must fail",
    )
    .to_string()
    .contains("truncated VMS JPEG component list"));

    for payload in [
        sof_payload(0, 16, 8, 0x11),
        sof_payload(128, 0, 8, 0x11),
        sof_payload(128, 16, 8, 0),
    ] {
        assert!(expect_error(
            parse_header_segments(&[(0xC0, &payload)]),
            "invalid SOF geometry must fail",
        )
        .to_string()
        .contains("invalid VMS JPEG dimensions or sampling"));
    }

    let short_dri = [0u8];
    assert!(expect_error(
        parse_header_segments(&[(0xDD, &short_dri)]),
        "short DRI must fail",
    )
    .to_string()
    .contains("truncated VMS JPEG DRI"));
}

#[test]
fn jpeg_header_requires_compatible_restart_geometry() {
    let sos: [u8; 0] = [];
    let dri_eight = 8u16.to_be_bytes();

    let error = expect_error(
        parse_header_segments(&[(0xDD, &dri_eight), (0xDA, &sos)]),
        "SOS without SOF must fail",
    );
    assert!(error.to_string().contains("missing SOF before SOS"));

    let sof = sof_payload(128, 16, 8, 0x11);
    let error = expect_error(
        parse_header_segments(&[(0xC0, &sof), (0xDA, &sos)]),
        "SOS without DRI must fail",
    );
    assert!(error.to_string().contains("missing restart interval"));

    let narrow = sof_payload(32, 16, 8, 0x11);
    let dri_five = 5u16.to_be_bytes();
    let error = expect_error(
        parse_header_segments(&[(0xC0, &narrow), (0xDD, &dri_five), (0xDA, &sos)]),
        "oversized restart interval must fail",
    );
    assert!(error.to_string().contains("greater than MCUs per row"));

    let unaligned = sof_payload(80, 16, 8, 0x11);
    let dri_six = 6u16.to_be_bytes();
    let error = expect_error(
        parse_header_segments(&[(0xC0, &unaligned), (0xDD, &dri_six), (0xDA, &sos)]),
        "unaligned restart interval must fail",
    );
    assert!(error.to_string().contains("does not align to MCU rows"));
}

#[test]
fn jpeg_header_keeps_first_comment_and_accepts_fill_markers() {
    let sof = sof_payload(128, 16, 8, 0x11);
    let dri = 8u16.to_be_bytes();
    let sos: [u8; 0] = [];
    let header = parse_header_segments(&[
        (0xFE, b"first\0ignored"),
        (0xFE, b"second"),
        (0xC0, &sof),
        (0xDD, &dri),
        (0xDA, &sos),
    ])
    .expect("valid synthetic VMS JPEG header");

    assert_eq!(header.comment.as_deref(), Some("first"));
    assert_eq!(header.geometry.width, 128);
    assert_eq!(header.geometry.tile_width, 64);
}

#[test]
fn sof_dimension_patch_checks_width_height_and_header_bounds() {
    let mut header = vec![0u8; 8];
    patch_sof_dimensions(&mut header, 2, 640, 480).expect("patch dimensions");
    assert_eq!(&header[2..6], &[1, 224, 2, 128]);
    assert!(patch_sof_dimensions(&mut header, 2, u32::from(u16::MAX) + 1, 1).is_err());
    assert!(patch_sof_dimensions(&mut header, 2, 1, u32::from(u16::MAX) + 1).is_err());
    assert!(patch_sof_dimensions(&mut header, 6, 1, 1).is_err());
    assert!(patch_sof_dimensions(&mut header, usize::MAX, 1, 1).is_err());
}

#[test]
fn restart_scanner_handles_stuffing_boundaries_eoi_and_unexpected_markers() {
    let temp = tempfile::tempdir().expect("temporary restart scanner directory");
    let path = temp.path().join("scan.bin");

    fs::write(&path, [1, 0xFF, 0, 2, 0xFF, 0xD3, 3]).expect("write restart stream");
    let mut file = File::open(&path).unwrap();
    assert_eq!(
        scan_one_restart_offset(&mut file, 7, &path).unwrap(),
        Some(6)
    );

    fs::write(&path, [0xFF, 0xD9]).expect("write EOI stream");
    let mut file = File::open(&path).unwrap();
    assert_eq!(scan_one_restart_offset(&mut file, 2, &path).unwrap(), None);

    fs::write(&path, [0xFF, 0xE1]).expect("write invalid marker stream");
    let mut file = File::open(&path).unwrap();
    assert!(scan_one_restart_offset(&mut file, 2, &path).is_err());

    let mut boundary = vec![0; JPEG_SCAN_CHUNK_BYTES - 1];
    boundary.extend_from_slice(&[0xFF, 0xD0]);
    fs::write(&path, &boundary).expect("write chunk-boundary restart stream");
    let mut file = File::open(&path).unwrap();
    assert_eq!(
        scan_one_restart_offset(&mut file, boundary.len() as u64, &path).unwrap(),
        Some(boundary.len() as u64)
    );
}

#[test]
fn restart_scanner_records_multiple_offsets_in_one_pass() {
    let temp = tempfile::tempdir().expect("temporary restart scanner directory");
    let path = temp.path().join("scan-many.bin");
    fs::write(
        &path,
        [1, 0xFF, 0xD0, 2, 0xFF, 0, 3, 0xFF, 0xD1, 4, 0xFF, 0xD2, 5],
    )
    .expect("write restart stream");

    let mut file = File::open(&path).expect("open restart stream");
    let mut offsets = [None; 3];
    assert_eq!(
        find_restart_offsets(&mut file, 14, &path, &mut offsets).unwrap(),
        3
    );
    assert_eq!(offsets, [Some(3), Some(9), Some(12)]);
}

#[test]
fn vms_jpeg_reports_invalid_scale_index_and_corrupt_entropy() {
    let temp = tempfile::tempdir().expect("temporary VMS JPEG directory");
    let path = temp.path().join("tile.jpg");
    write_restart_jpeg(&path, 128, 16);

    let jpeg = VmsJpeg::parse(&path, Vec::new()).expect("parse restart JPEG");
    assert!(jpeg.decode_tile(0, 3, BackendRequest::Auto).is_err());
    assert!(jpeg.decode_tile(4, 1, BackendRequest::Auto).is_err());
    assert!(!jpeg.valid_recorded_restart_offset(0).unwrap());
    assert!(jpeg
        .valid_recorded_restart_offset(jpeg.header.len() as u64)
        .unwrap());
    assert!(!jpeg
        .valid_recorded_restart_offset(jpeg.file_len + 1)
        .unwrap());

    let mut corrupt = VmsJpeg::parse(&path, Vec::new()).expect("parse corruptible JPEG");
    corrupt.header[0] = 0;
    assert!(corrupt.decode_tile(0, 1, BackendRequest::Auto).is_err());

    let truncated = VmsJpeg::parse(&path, Vec::new()).expect("parse truncatable JPEG");
    fs::write(&path, &truncated.header).expect("truncate JPEG after parse");
    assert!(truncated.decode_tile(0, 1, BackendRequest::Auto).is_err());
}

#[test]
fn poisoned_vms_jpeg_mutexes_recover_without_changing_pixels() {
    let temp = tempfile::tempdir().expect("temporary poisoned JPEG directory");
    let path = temp.path().join("tile.jpg");
    write_restart_jpeg(&path, 128, 16);
    let jpeg = VmsJpeg::parse(&path, Vec::new()).expect("parse restart JPEG");

    poison(&jpeg.decoded_tile_cache);
    poison(&jpeg.file);
    let tile = jpeg
        .decode_tile(3, 1, BackendRequest::Auto)
        .expect("decode with recovered poisoned mutexes");
    assert_eq!((tile.width(), tile.height()), (64, 8));
}

#[test]
fn tile_read_reports_file_read_and_entropy_marker_failures() {
    let temp = tempfile::tempdir().expect("temporary mutable JPEG directory");
    let path = temp.path().join("tile.jpg");
    write_restart_jpeg(&path, 128, 16);

    let read_failure = VmsJpeg::parse(&path, Vec::new()).expect("parse restart JPEG");
    let write_only = OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("write-only JPEG");
    *read_failure.file.lock().unwrap() = write_only;
    assert!(matches!(
        read_failure.decode_tile(0, 1, BackendRequest::Auto),
        Err(WsiError::IoWithPath { .. })
    ));

    write_restart_jpeg(&path, 128, 16);
    let marker_failure = VmsJpeg::parse(&path, Vec::new()).expect("parse restart JPEG");
    let (_, stop) = marker_failure.tile_entropy_bounds(0).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&path).unwrap();
    writer.seek(SeekFrom::Start(stop - 2)).unwrap();
    writer.write_all(&[0]).unwrap();
    writer.flush().unwrap();
    assert!(marker_failure
        .decode_tile(0, 1, BackendRequest::Auto)
        .unwrap_err()
        .to_string()
        .contains("does not end at a marker"));
}

#[test]
fn parse_wraps_header_failures_with_slide_path_context() {
    let temp = tempfile::tempdir().expect("temporary invalid VMS JPEG directory");
    let path = temp.path().join("invalid.jpg");
    fs::write(&path, b"not a JPEG").expect("write invalid JPEG");

    let error = expect_error(VmsJpeg::parse(&path, Vec::new()), "invalid JPEG must fail");
    assert!(matches!(
        error,
        WsiError::InvalidSlide {
            path: error_path,
            message,
        } if error_path == path
            && message.contains("failed to derive VMS JPEG tile geometry")
            && message.contains("missing SOI marker")
    ));
}

#[test]
fn single_tile_reads_report_bounds_and_file_handle_failures_at_the_tile_boundary() {
    let temp = tempfile::tempdir().expect("temporary single-tile JPEG directory");
    let path = temp.path().join("single.jpg");
    write_restart_jpeg(&path, 64, 8);

    let mut invalid_bounds = VmsJpeg::parse(&path, Vec::new()).expect("parse one-tile JPEG");
    invalid_bounds.file_len = 0;
    let error = invalid_bounds
        .tile_jpeg_bytes(0, 64, 8)
        .expect_err("backwards entropy bounds must fail");
    assert!(error
        .to_string()
        .contains("invalid VMS JPEG entropy bounds"));

    let read_failure = VmsJpeg::parse(&path, Vec::new()).expect("parse readable JPEG");
    let write_only = OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open write-only JPEG");
    *read_failure.file.lock().unwrap() = write_only;
    assert!(matches!(
        read_failure.tile_jpeg_bytes(0, 64, 8),
        Err(WsiError::IoWithPath { path: error_path, .. }) if error_path == path
    ));

    #[cfg(unix)]
    {
        let seek_failure = VmsJpeg::parse(&path, Vec::new()).expect("parse seek fixture");
        let directory = File::open(temp.path()).expect("open directory handle");
        *seek_failure.file.lock().unwrap() = directory;
        assert!(matches!(
            seek_failure.tile_jpeg_bytes(0, 64, 8),
            Err(WsiError::IoWithPath { path: error_path, .. }) if error_path == path
        ));
    }
}

#[test]
fn mcu_index_recovery_handles_uninitialized_and_poisoned_state() {
    let temp = tempfile::tempdir().expect("temporary MCU recovery directory");
    let path = temp.path().join("tiles.jpg");
    write_restart_jpeg(&path, 128, 16);
    let jpeg = VmsJpeg::parse(&path, Vec::new()).expect("parse restart JPEG");

    let mut uninitialized = vec![None; jpeg.mcu_starts.lock().unwrap().len()];
    jpeg.ensure_mcu_start(&mut uninitialized, 0)
        .expect("initialize first MCU offset");
    assert_eq!(uninitialized[0], Some(jpeg.header.len() as u64));

    let (_, restart_offset) = jpeg
        .tile_entropy_bounds(0)
        .expect("discover first restart marker");
    poison(&jpeg.mcu_starts);
    assert_eq!(
        jpeg.tile_entropy_bounds(0)
            .expect("recover poisoned MCU index")
            .1,
        restart_offset
    );

    poison(&jpeg.file);
    assert!(jpeg
        .valid_recorded_restart_offset(restart_offset)
        .expect("recover poisoned file mutex"));
}

#[test]
fn header_and_restart_scanners_cover_resource_and_fill_marker_boundaries() {
    let temp = tempfile::tempdir().expect("temporary scanner boundary directory");
    let path = temp.path().join("scanner.bin");
    fs::write(&path, [0xFF, 0xFF, 0xD0]).expect("write fill-marker stream");

    let mut file = File::open(&path).expect("open marker stream");
    let mut header = Vec::new();
    assert_eq!(
        read_next_header_marker(&mut file, &path, &mut header).unwrap(),
        0xD0
    );

    let mut file = File::open(&path).expect("open restart stream");
    assert_eq!(scan_one_restart_offset(&mut file, 0, &path).unwrap(), None);
    assert_eq!(
        scan_one_restart_offset(&mut file, 3, &path).unwrap(),
        Some(3)
    );

    let mut file = File::open(&path).expect("open header limit stream");
    let mut oversized_header = vec![0; JPEG_HEADER_MAX_BYTES];
    let mut byte = [0u8; 1];
    let error = read_exact_header(&mut file, &path, &mut oversized_header, &mut byte)
        .expect_err("header limit must fail before reading");
    assert!(error.to_string().contains("header exceeds"));
}

#[cfg(unix)]
#[test]
fn nonseekable_and_write_only_handles_preserve_vms_io_context() {
    fn socket_file() -> File {
        let (socket, peer) = UnixStream::pair().expect("create socket pair");
        drop(peer);
        let descriptor: OwnedFd = socket.into();
        File::from(descriptor)
    }

    let temp = tempfile::tempdir().expect("temporary nonseekable JPEG directory");
    let path = temp.path().join("tiles.jpg");
    write_restart_jpeg(&path, 128, 16);

    let jpeg = VmsJpeg::parse(&path, Vec::new()).expect("parse restart JPEG");
    *jpeg.file.lock().unwrap() = socket_file();
    assert!(matches!(
        jpeg.tile_jpeg_bytes(0, 64, 8),
        Err(WsiError::IoWithPath { path: error_path, .. }) if error_path == path
    ));

    let jpeg = VmsJpeg::parse(&path, Vec::new()).expect("parse MCU seek fixture");
    *jpeg.file.lock().unwrap() = socket_file();
    let mut starts = vec![Some(jpeg.header.len() as u64), None];
    assert!(matches!(
        jpeg.ensure_mcu_start(&mut starts, 1),
        Err(WsiError::IoWithPath { path: error_path, .. }) if error_path == path
    ));

    let jpeg = VmsJpeg::parse(&path, Vec::new()).expect("parse recorded-offset seek fixture");
    *jpeg.file.lock().unwrap() = socket_file();
    assert!(matches!(
        jpeg.valid_recorded_restart_offset(jpeg.header.len() as u64 + 2),
        Err(WsiError::IoWithPath { path: error_path, .. }) if error_path == path
    ));

    let jpeg = VmsJpeg::parse(&path, Vec::new()).expect("parse recorded-offset read fixture");
    let write_only = OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open write-only JPEG");
    *jpeg.file.lock().unwrap() = write_only;
    assert!(matches!(
        jpeg.valid_recorded_restart_offset(jpeg.header.len() as u64 + 2),
        Err(WsiError::IoWithPath { path: error_path, .. }) if error_path == path
    ));

    let mut socket = socket_file();
    assert!(matches!(
        scan_one_restart_offset(&mut socket, u64::MAX, &path),
        Err(WsiError::IoWithPath { path: error_path, .. }) if error_path == path
    ));
}

#[test]
fn decoder_reports_entropy_that_disagrees_with_the_declared_tile_width() {
    let temp = tempfile::tempdir().expect("temporary mismatched JPEG directory");
    let path = temp.path().join("tiles.jpg");
    write_restart_jpeg(&path, 128, 16);
    let mut jpeg = VmsJpeg::parse(&path, Vec::new()).expect("parse restart JPEG");
    jpeg.width = 65;
    jpeg.tile_width = 65;

    let error = jpeg
        .decode_tile(0, 1, BackendRequest::Auto)
        .expect_err("declared tile width must agree with its entropy stream");
    assert!(matches!(error, WsiError::Jpeg(_)));
}
