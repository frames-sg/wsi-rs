use std::fs;

use super::super::*;
use super::fixtures::MiraxFixture;

fn error<T>(result: Result<T, WsiError>) -> WsiError {
    match result {
        Ok(_) => panic!("malformed MIRAX fixture unexpectedly succeeded"),
        Err(error) => error,
    }
}

fn invalid_message(error: WsiError) -> String {
    match error {
        WsiError::InvalidSlide { message, .. } => message,
        other => panic!("expected InvalidSlide, got {other:?}"),
    }
}

fn mutate_slidedat(from: &str, to: &str) -> WsiError {
    let fixture = MiraxFixture::complete();
    let source = fixture.complete_slidedat();
    assert!(source.contains(from), "fixture mutation source must exist");
    fixture.write_slidedat(&source.replacen(from, to, 1));
    error(MiraxSlide::parse(&fixture.path))
}

fn read_u32(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}

fn set_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn probe_and_entry_resolution_reject_wrong_or_incomplete_bundles() {
    let backend = MiraxBackend::new();
    let temp = tempfile::tempdir().unwrap();
    let wrong_extension = temp.path().join("slide.txt");
    fs::write(&wrong_extension, b"not MIRAX").unwrap();
    assert!(!backend.probe(&wrong_extension).unwrap().detected);
    assert!(matches!(
        error(backend.open(&wrong_extension)),
        WsiError::UnsupportedFormat(_)
    ));

    let missing_companion = temp.path().join("missing.mrxs");
    fs::write(&missing_companion, b"").unwrap();
    assert!(!backend.probe(&missing_companion).unwrap().detected);
    assert!(
        invalid_message(error(MiraxSlide::parse(&missing_companion)))
            .contains("missing MIRAX directory")
    );

    let missing_ini = temp.path().join("empty.mrxs");
    fs::write(&missing_ini, b"").unwrap();
    fs::create_dir(temp.path().join("empty")).unwrap();
    assert!(!backend.probe(&missing_ini).unwrap().detected);
    assert!(matches!(
        error(MiraxSlide::parse(&missing_ini)),
        WsiError::IoWithPath { .. }
    ));

    let fixture = MiraxFixture::complete();
    assert!(looks_like_mirax(&fixture.path));
    assert_eq!(
        slide_dir_from_entry(&fixture.path).unwrap(),
        fixture.slide_dir
    );
}

#[test]
fn probe_recognizes_a_mirax_bundle_whose_metadata_is_corrupt() {
    let fixture = MiraxFixture::complete();
    let source = fixture.complete_slidedat();
    fixture.write_slidedat(&source.replacen("IMAGENUMBER_X=4", "IMAGENUMBER_X=0", 1));
    let backend = MiraxBackend::new();

    let probe = backend.probe(&fixture.path).expect("probe corrupt MIRAX");
    assert!(probe.detected);
    assert_eq!(probe.vendor, "mirax");
    assert!(matches!(
        error(backend.open(&fixture.path)),
        WsiError::InvalidSlide { .. }
    ));
}

#[test]
fn open_rejects_invalid_ini_structure_counts_and_level_geometry() {
    let cases = [
        ("[GENERAL]", "[BROKEN_GENERAL]", "missing [GENERAL] group"),
        (
            "[HIERARCHICAL]",
            "[BROKEN_HIERARCHICAL]",
            "missing [HIERARCHICAL] group",
        ),
        (
            "[DATAFILE]",
            "[BROKEN_DATAFILE]",
            "missing [DATAFILE] group",
        ),
        ("IMAGENUMBER_X=4", "IMAGENUMBER_X=0", "must be positive"),
        (
            "CameraImageDivisionsPerSide=1",
            "CameraImageDivisionsPerSide=0",
            "must be positive",
        ),
        (
            "CameraImageDivisionsPerSide=1",
            "CameraImageDivisionsPerSide=8",
            "grid exceeds",
        ),
        ("HIER_COUNT=1", "HIER_COUNT=0", "hierarchy counts"),
        ("NONHIER_COUNT=2", "NONHIER_COUNT=-1", "hierarchy counts"),
        (
            "HIER_0_NAME=Slide zoom level",
            "HIER_0_NAME=Other hierarchy",
            "cannot find Slide zoom level",
        ),
        ("HIER_0_COUNT=3", "HIER_0_COUNT=0", "no zoom levels"),
        ("FILE_COUNT=1", "FILE_COUNT=0", "no data files"),
        (
            "FILE_COUNT=1",
            "FILE_COUNT=2\nFILE_1=Data0000.dat",
            "duplicate MIRAX data file",
        ),
        (
            "IMAGE_CONCAT_FACTOR=0",
            "IMAGE_CONCAT_FACTOR=-1",
            "invalid IMAGE_CONCAT_FACTOR",
        ),
        (
            "IMAGE_CONCAT_FACTOR=1\nIMAGE_FORMAT=PNG",
            "IMAGE_CONCAT_FACTOR=0\nIMAGE_FORMAT=PNG",
            "invalid IMAGE_CONCAT_FACTOR",
        ),
        (
            "IMAGE_CONCAT_FACTOR=1\nIMAGE_FORMAT=BMP24",
            "IMAGE_CONCAT_FACTOR=29\nIMAGE_FORMAT=BMP24",
            "concat exponent too large",
        ),
        ("DIGITIZER_WIDTH=16", "DIGITIZER_WIDTH=0", "zero digitizer"),
        ("OVERLAP_X=0", "OVERLAP_X=16", "invalid MIRAX tile advance"),
        (
            "MICROMETER_PER_PIXEL_X=0.25",
            "MICROMETER_PER_PIXEL_X=invalid",
            "invalid MIRAX float",
        ),
    ];

    for (from, to, expected) in cases {
        let message = invalid_message(mutate_slidedat(from, to));
        assert!(
            message.contains(expected),
            "{message:?} did not contain {expected:?}"
        );
    }

    assert!(matches!(
        mutate_slidedat("IMAGE_FORMAT=JPEG", "IMAGE_FORMAT=GIF"),
        WsiError::DisplayConversion(message) if message.contains("GIF")
    ));
    let message = invalid_message(mutate_slidedat(
        "THUMBNAIL_IMAGE_TYPE=JPEG",
        "THUMBNAIL_IMAGE_TYPE=PNG",
    ));
    assert!(message.contains("unsupported MIRAX associated image format PNG"));
}

#[test]
fn open_rejects_nonzero_slide_hierarchy_and_missing_required_keys() {
    let error = mutate_slidedat(
        "HIER_COUNT=1\nNONHIER_COUNT=2\nHIER_0_NAME=Slide zoom level",
        "HIER_COUNT=2\nNONHIER_COUNT=2\nHIER_0_NAME=Other\nHIER_1_NAME=Slide zoom level",
    );
    assert!(invalid_message(error).contains("Slide zoom level not HIER_0"));

    for key_line in [
        "SLIDE_ID=SYNTHETIC\n",
        "INDEXFILE=Index.dat\n",
        "HIER_0_VAL_0_SECTION=LEVEL_0\n",
        "FILE_0=Data0000.dat\n",
        "PREVIEW_IMAGE_TYPE=JPEG\n",
    ] {
        let error = mutate_slidedat(key_line, "");
        assert!(invalid_message(error).contains("missing MIRAX"));
    }
}

#[test]
fn index_truncation_pointers_pages_and_records_fail_with_context() {
    let fixture = MiraxFixture::complete();
    let mut bytes = fixture.read_index();
    bytes.truncate(4);
    fixture.write_index(&bytes);
    assert!(matches!(
        error(MiraxSlide::parse(&fixture.path)),
        WsiError::IoWithPath { .. }
    ));

    let fixture = MiraxFixture::complete();
    let mut bytes = fixture.read_index();
    set_i32(&mut bytes, 14, -1);
    fixture.write_index(&bytes);
    assert!(
        invalid_message(error(MiraxSlide::parse(&fixture.path))).contains("negative MIRAX pointer")
    );

    for mutation in [
        "head",
        "page_len",
        "cycle",
        "negative_record",
        "row",
        "file",
    ] {
        let fixture = MiraxFixture::complete();
        let mut bytes = fixture.read_index();
        let hierarchy_table = read_u32(&bytes, 14);
        let head = read_u32(&bytes, hierarchy_table);
        let page = read_u32(&bytes, head + 4);
        match mutation {
            "head" => set_i32(&mut bytes, head, 1),
            "page_len" => set_i32(&mut bytes, page, -1),
            "cycle" => set_i32(&mut bytes, page + 4, page as i32),
            "negative_record" => set_i32(&mut bytes, page + 8, -1),
            "row" => set_i32(&mut bytes, page + 8, 16),
            "file" => set_i32(&mut bytes, page + 20, 1),
            _ => unreachable!(),
        }
        fixture.write_index(&bytes);
        assert!(matches!(
            error(MiraxSlide::parse(&fixture.path)),
            WsiError::InvalidSlide { .. } | WsiError::IoWithPath { .. }
        ));
    }

    let fixture = MiraxFixture::complete();
    let mut bytes = fixture.read_index();
    let hierarchy_table = read_u32(&bytes, 14);
    let second_head = read_u32(&bytes, hierarchy_table + 4);
    let second_page = read_u32(&bytes, second_head + 4);
    set_i32(&mut bytes, second_page + 8, 1);
    fixture.write_index(&bytes);
    assert!(invalid_message(error(MiraxSlide::parse(&fixture.path))).contains("not aligned"));
}

#[test]
fn nonhierarchical_pages_and_truncated_associated_payloads_are_rejected() {
    for mutation in ["empty", "header", "negative", "file"] {
        let fixture = MiraxFixture::complete();
        let mut bytes = fixture.read_index();
        let nonhier_table = read_u32(&bytes, 18);
        let macro_head = read_u32(&bytes, nonhier_table + 4);
        let macro_page = read_u32(&bytes, macro_head + 4);
        match mutation {
            "empty" => set_i32(&mut bytes, macro_page, 0),
            "header" => set_i32(&mut bytes, macro_page + 8, 1),
            "negative" => set_i32(&mut bytes, macro_page + 16, -1),
            "file" => set_i32(&mut bytes, macro_page + 24, 1),
            _ => unreachable!(),
        }
        fixture.write_index(&bytes);
        assert!(matches!(
            error(MiraxSlide::parse(&fixture.path)),
            WsiError::InvalidSlide { .. }
        ));
    }

    let fixture = MiraxFixture::complete();
    let len = fs::metadata(&fixture.data_path).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(&fixture.data_path)
        .unwrap()
        .set_len(len - 1)
        .unwrap();
    let message = invalid_message(error(MiraxSlide::parse(&fixture.path)));
    assert!(message.contains("associated image thumbnail dimensions"));
}

#[test]
fn bounded_slidedat_and_missing_index_are_not_silently_accepted() {
    let fixture = MiraxFixture::complete();
    fixture.write_slidedat(&"x".repeat((SLIDEDAT_MAX_SIZE + 1) as usize));
    assert!(invalid_message(error(MiraxSlide::parse(&fixture.path))).contains("too large"));

    let fixture = MiraxFixture::complete();
    fs::remove_file(&fixture.index_path).unwrap();
    let message = invalid_message(error(MiraxSlide::parse(&fixture.path)));
    assert!(message.contains("Index.dat"));
    assert!(message.contains("cannot resolve companion file"));
}
