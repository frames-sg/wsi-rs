use super::super::*;
use super::fixtures::synthetic_dri_420_jpeg_header;

#[test]
fn probe_jpeg_geometry_via_j2k_matches_synthetic_header() {
    let probe = probe_jpeg_geometry_bytes_via_j2k(synthetic_dri_420_jpeg_header())
        .expect("j2k should inspect synthetic DRI JPEG header");
    assert_eq!(probe.restart_interval, 10);
    assert_eq!(probe.mcu_w, 16);
    assert_eq!(probe.mcu_h, 16);
}

#[test]
fn probe_jpeg_geometry_accepts_ndpi_zero_sof_dimensions() {
    let mut header = synthetic_dri_420_jpeg_header();
    let sof = header
        .windows(2)
        .position(|bytes| bytes == [0xFF, 0xC0])
        .expect("synthetic header has SOF0");
    header[sof + 5..sof + 9].copy_from_slice(&[0, 0, 0, 0]);

    let probe = probe_jpeg_geometry_bytes_via_j2k(header)
        .expect("NDPI lenient probe should accept zero SOF dimensions");

    assert_eq!(probe.restart_interval, 10);
    assert_eq!(probe.mcu_w, 16);
    assert_eq!(probe.mcu_h, 16);
    assert!(probe.header.len() < JPEG_HEADER_PROBE_BYTES as usize);
}

#[test]
fn probe_jpeg_geometry_reports_strict_and_lenient_failures() {
    let error = match probe_jpeg_geometry_bytes_via_j2k(vec![0x00, 0x00]) {
        Ok(_) => panic!("invalid JPEG header should fail both geometry probes"),
        Err(error) => error,
    };
    let message = error.to_string();

    assert!(message.contains("cannot parse JPEG geometry with j2k"));
    assert!(message.contains("lenient NDPI probe failed"));
    assert!(message.contains("NDPI JPEG header missing SOI"));
}

#[test]
fn lenient_probe_rejects_scan_before_frame_geometry() {
    let result = probe_jpeg_geometry_bytes_lenient(&[
        0xFF, 0xD8, // SOI
        0xFF, 0xDA, 0x00, 0x02, // SOS without a preceding SOF
    ]);
    let error = match result {
        Ok(_) => panic!("SOS without SOF geometry should be rejected"),
        Err(error) => error,
    };

    assert!(error
        .to_string()
        .contains("NDPI JPEG header missing SOF marker"));
}

#[test]
fn ndpi_power_of_two_factor_requires_exact_power_of_two_dimensions() {
    assert_eq!(
        ndpi_power_of_two_factor((51200, 38144), (12800, 9536)),
        Some(4)
    );
    assert_eq!(
        ndpi_power_of_two_factor((51200, 38144), (25600, 19072)),
        Some(2)
    );
    assert_eq!(
        ndpi_power_of_two_factor((51200, 38144), (200, 149)),
        Some(256)
    );
    assert_eq!(ndpi_power_of_two_factor((51200, 38144), (74, 55)), None);
    assert_eq!(ndpi_power_of_two_factor((51200, 38144), (3200, 2400)), None);
}
