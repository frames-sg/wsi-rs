//! Native-pixel comparisons against separately decoded public vendor samples.

mod support;

use std::path::PathBuf;

use serde::Deserialize;
use support::compare::{compare_rgba, tolerance_failure, Tolerance};
use wsi_rs::{CpuTileData, PlaneSelection, Slide};

#[derive(Deserialize)]
struct Probe {
    path: PathBuf,
    origin: (i64, i64),
    size: (u32, u32),
    channel: u32,
    sample_type: String,
    pixels: String,
}

#[test]
#[ignore = "requires independently decoded probes from scripts/prepare-vendor-reference.py"]
fn vendor_native_pixels_match_independent_reference() {
    let root = std::env::var_os("WSI_RS_VENDOR_REFERENCE_ROOT")
        .map(PathBuf::from)
        .expect("set WSI_RS_VENDOR_REFERENCE_ROOT to the generated reference directory");
    let probes: Vec<Probe> = serde_json::from_slice(
        &std::fs::read(root.join("cases.json")).expect("read independent reference cases"),
    )
    .expect("parse independent reference cases");
    assert!(
        !probes.is_empty(),
        "independent reference cases must not be empty"
    );
    for probe in probes {
        let slide = Slide::open(&probe.path).expect("open public vendor sample");
        let actual = slide
            .read_region(&support::region_request(
                0,
                0,
                0,
                PlaneSelection::new(0, probe.channel, 0),
                probe.origin.0,
                probe.origin.1,
                probe.size.0,
                probe.size.1,
            ))
            .expect("read native vendor pixels");
        assert_eq!((actual.width(), actual.height()), probe.size);
        let expected = std::fs::read(root.join(&probe.pixels)).expect("read reference pixels");
        match probe.sample_type.as_str() {
            "u16" => {
                let CpuTileData::U16(values) = actual.data() else {
                    panic!("{}: expected native U16 pixels", probe.pixels);
                };
                let bytes: Vec<u8> = values
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect();
                assert_eq!(
                    bytes, expected,
                    "{}: native U16 pixels differ",
                    probe.pixels
                );
            }
            "u8" => {
                assert_eq!(
                    expected.len(),
                    probe.size.0 as usize * probe.size.1 as usize * 3
                );
                let expected: Vec<u8> = expected
                    .chunks_exact(3)
                    .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
                    .collect();
                let actual = actual.into_rgba().expect("RGB8 display pixels");
                let report =
                    compare_rgba(actual.as_raw(), &expected, Tolerance::JPEG_DECODER_COMPAT);
                assert!(
                    report.passed,
                    "{}",
                    tolerance_failure(&probe.pixels, &report).unwrap()
                );
            }
            other => panic!("unsupported reference sample type {other}"),
        }
    }
}
