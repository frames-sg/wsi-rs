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
