use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::super::ini::{parse_image_key_suffix, parse_u32, parse_vms_ini, parse_vms_opt_offsets};
use super::super::*;
use super::fixtures::VmsFixture;

fn expect_wsi_error<T>(result: Result<T, WsiError>, context: &str) -> WsiError {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

fn invalid_slide_message(error: WsiError) -> String {
    match error {
        WsiError::InvalidSlide { message, .. } => message,
        other => panic!("expected InvalidSlide, got {other:?}"),
    }
}

#[test]
fn probe_rejects_unreadable_missing_group_and_zero_grid_keys() {
    let temp = tempfile::tempdir().expect("temporary VMS probe directory");
    let backend = HamamatsuVmsBackend::new();

    for (name, contents) in [
        ("missing.vms", None),
        (
            "other.vms",
            Some("[Other]\nNoJpegColumns=1\nNoJpegRows=1\n"),
        ),
        (
            "zero.vms",
            Some("[Virtual Microscope Specimen]\nNoJpegColumns=0\nNoJpegRows=1\n"),
        ),
        (
            "invalid.vms",
            Some("[Virtual Microscope Specimen]\nNoJpegColumns=nope\nNoJpegRows=1\n"),
        ),
    ] {
        let path = temp.path().join(name);
        if let Some(contents) = contents {
            fs::write(&path, contents).expect("write negative VMS probe fixture");
        }
        let result = backend.probe(&path).expect("negative VMS probe");
        assert!(!result.detected, "{name} unexpectedly detected");
        assert!(result.vendor.is_empty());
        assert_eq!(result.confidence, ProbeConfidence::Likely);
    }
}

#[test]
fn open_reports_missing_group_dimensions_and_shards() {
    let fixture = VmsFixture::complete();
    let cases = [
        (
            "[Other]\nvalue=1\n",
            "missing [Virtual Microscope Specimen] group",
        ),
        (
            "[Virtual Microscope Specimen]\nNoJpegRows=1\n",
            "missing NoJpegColumns",
        ),
        (
            "[Virtual Microscope Specimen]\nNoJpegColumns=x\nNoJpegRows=1\n",
            "invalid integer for NoJpegColumns",
        ),
        (
            "[Virtual Microscope Specimen]\nNoJpegColumns=1\nNoJpegRows=0\n",
            "VMS file has no columns or rows",
        ),
        (
            "[Virtual Microscope Specimen]\nNoJpegColumns=1\nNoJpegRows=1\nMapFile=map.jpg\n",
            "missing VMS image filename 0",
        ),
        (
            "[Virtual Microscope Specimen]\nNoJpegColumns=65537\nNoJpegRows=1\n",
            "VMS JPEG shard count exceeds safety limit",
        ),
        (
            "[Virtual Microscope Specimen]\nNoJpegColumns=1\nNoJpegRows=1\nImageFile=image0.jpg\n",
            "missing MapFile",
        ),
    ];

    for (key, expected) in cases {
        fixture.write_key(key);
        let message = invalid_slide_message(expect_wsi_error(
            HamamatsuVmsBackend::new().open(&fixture.path),
            expected,
        ));
        assert!(message.contains(expected), "unexpected error: {message}");
    }
}

#[test]
fn open_rejects_invalid_duplicate_and_unsafe_image_mappings() {
    let fixture = VmsFixture::complete();
    let cases = [
        (
            "[Virtual Microscope Specimen]\nNoJpegColumns=1\nNoJpegRows=1\nImageFile(1,0)=image0.jpg\nMapFile=map.jpg\n",
            "invalid VMS image coordinates (1,0)",
        ),
        (
            "[Virtual Microscope Specimen]\nNoJpegColumns=1\nNoJpegRows=1\nImageFile=image0.jpg\nImageFile(0,0)=image1.jpg\nMapFile=map.jpg\n",
            "duplicate VMS image for (0,0)",
        ),
        (
            "[Virtual Microscope Specimen]\nNoJpegColumns=2\nNoJpegRows=1\nImageFile(0,0)=image0.jpg\nImageFile(1,0)=image0.jpg\nMapFile=map.jpg\n",
            "duplicate VMS image file path",
        ),
        (
            "[Virtual Microscope Specimen]\nNoJpegColumns=1\nNoJpegRows=1\nImageFile=../image0.jpg\nMapFile=map.jpg\n",
            "invalid companion path",
        ),
    ];

    for (key, expected) in cases {
        fixture.write_key(key);
        let message = invalid_slide_message(expect_wsi_error(
            HamamatsuVmsBackend::new().open(&fixture.path),
            expected,
        ));
        assert!(message.contains(expected), "unexpected error: {message}");
    }
}

#[test]
fn image_key_suffix_parses_supported_forms_and_rejects_malformed_coordinates() {
    let path = Path::new("synthetic.vms");
    let plain = parse_image_key_suffix(path, "ImageFile").expect("plain image key");
    assert_eq!((plain.layer, plain.col, plain.row), (0, 0, 0));
    let grid = parse_image_key_suffix(path, "ImageFile(2,3)").expect("grid image key");
    assert_eq!((grid.layer, grid.col, grid.row), (0, 2, 3));
    let layered = parse_image_key_suffix(path, "ImageFile(4,2,3)").expect("layered image key");
    assert_eq!((layered.layer, layered.col, layered.row), (4, 2, 3));

    for key in [
        "ImageFile2,3",
        "ImageFile(1)",
        "ImageFile(a,2)",
        "ImageFile(1,a)",
        "ImageFile(a,1,2)",
        "ImageFile(1,a,2)",
        "ImageFile(1,2,a)",
    ] {
        assert!(parse_image_key_suffix(path, key).is_err(), "accepted {key}");
    }

    let mut group = HashMap::new();
    assert!(parse_u32(path, &group, "Count").is_err());
    group.insert("Count".into(), "bad".into());
    assert!(parse_u32(path, &group, "Count").is_err());
    group.insert("Count".into(), "42".into());
    assert_eq!(parse_u32(path, &group, "Count").unwrap(), 42);
}

#[test]
fn vms_key_file_enforces_its_bounded_input_limit() {
    let temp = tempfile::tempdir().expect("temporary oversized VMS directory");
    let path = temp.path().join("oversized.vms");
    fs::write(&path, vec![b'x'; (64 << 10) + 1]).expect("write oversized VMS key");
    assert!(matches!(
        parse_vms_ini(&path),
        Err(WsiError::InvalidSlide { message, .. }) if message == "VMS key file too large"
    ));
}

#[test]
fn optimisation_offsets_use_complete_rows_and_discard_truncation() {
    let fixture = VmsFixture::complete();
    let offsets = parse_vms_opt_offsets(Some(&fixture.opt_path), &fixture.image_paths)
        .expect("parse complete VMS optimisation file");
    assert_eq!(offsets.len(), 2);
    assert!(offsets.iter().all(|rows| rows.len() == 2));
    assert!(offsets.iter().flatten().all(Option::is_some));

    assert_eq!(
        parse_vms_opt_offsets(None, &fixture.image_paths).unwrap(),
        vec![Vec::new(), Vec::new()]
    );
    fs::write(&fixture.opt_path, [0u8; 41]).expect("truncate optimisation fixture");
    assert_eq!(
        parse_vms_opt_offsets(Some(&fixture.opt_path), &fixture.image_paths).unwrap(),
        vec![Vec::new(), Vec::new()]
    );
}

#[test]
fn complete_fixture_paths_are_regular_companions() {
    let fixture = VmsFixture::complete();
    assert!(fixture.map_path.is_file());
    assert!(fixture.macro_path.is_file());
}
