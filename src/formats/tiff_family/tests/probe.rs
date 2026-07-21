use super::super::*;
use super::fixtures::{build_aperio_tiff, build_ndpi_tiff};
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn probe_detects_ndpi() {
    let file = build_ndpi_tiff(&[(1024, 768, 40.0)]);
    let backend = TiffFamilyBackend::new();
    let result = backend.probe(file.path()).unwrap();

    assert!(result.detected);
    assert_eq!(result.vendor, "hamamatsu-ndpi");
    assert_eq!(result.confidence, ProbeConfidence::Definite);
}

#[test]
fn probe_rejects_non_tiff() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"this is not a tiff file at all").unwrap();
    file.flush().unwrap();

    let backend = TiffFamilyBackend::new();
    let result = backend.probe(file.path());

    // Non-TIFF files should return detected=false, not an error.
    // This lets other backends in the registry try their probes.
    let probe_result = result.unwrap();
    assert!(!probe_result.detected);
}

#[test]
fn probe_reports_malformed_ndpi_instead_of_hiding_parse_error() {
    let mut file = tempfile::Builder::new().suffix(".ndpi").tempfile().unwrap();
    file.write_all(b"II").unwrap();
    file.write_all(&42u16.to_le_bytes()).unwrap();
    file.write_all(&1024u32.to_le_bytes()).unwrap();
    file.flush().unwrap();

    let backend = TiffFamilyBackend::new();
    let err = backend
        .probe(file.path())
        .expect_err("malformed .ndpi should surface parser error");

    assert!(
        err.to_string().contains("first IFD offset")
            || err.to_string().contains("Error reading TIFF"),
        "got: {err}"
    );
}

#[test]
fn probe_rejects_plain_tiff_without_ndpi() {
    // Build a valid TIFF but without NDPI marker tag
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());

    let ifd_offset = 8u32;
    buf.extend_from_slice(&ifd_offset.to_le_bytes());

    // Simple IFD with just IMAGE_WIDTH and IMAGE_LENGTH
    let entry_count = 2u16;
    buf.extend_from_slice(&entry_count.to_le_bytes());

    // Tag 256 IMAGE_WIDTH
    buf.extend_from_slice(&256u16.to_le_bytes());
    buf.extend_from_slice(&4u16.to_le_bytes()); // LONG
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&512u32.to_le_bytes());

    // Tag 257 IMAGE_LENGTH
    buf.extend_from_slice(&257u16.to_le_bytes());
    buf.extend_from_slice(&4u16.to_le_bytes()); // LONG
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&384u32.to_le_bytes());

    // Next IFD offset = 0 (end of chain)
    buf.extend_from_slice(&0u32.to_le_bytes());

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();

    let backend = TiffFamilyBackend::new();
    let result = backend.probe(file.path()).unwrap();

    assert!(!result.detected);
    assert!(result.vendor.is_empty());
}

// ── Registration order tests ────────────────────────────────────

#[test]
fn probe_detects_aperio() {
    let file = build_aperio_tiff(1024, 768);
    let backend = TiffFamilyBackend::new();
    let result = backend.probe(file.path()).unwrap();
    assert!(result.detected);
    assert_eq!(result.vendor, "aperio");
}

#[test]
fn specific_vendor_beats_generic() {
    // An Aperio-like file should be detected as "aperio", not "generic-tiff"
    let file = build_aperio_tiff(512, 384);
    let backend = TiffFamilyBackend::new();
    let result = backend.probe(file.path()).unwrap();
    assert!(result.detected);
    assert_eq!(result.vendor, "aperio");
}

#[test]
fn ndpi_still_detected_first() {
    // Regression test: NDPI files should still be detected, not caught by generic
    let file = build_ndpi_tiff(&[(2048, 1536, 40.0)]);
    let backend = TiffFamilyBackend::new();
    let result = backend.probe(file.path()).unwrap();
    assert!(result.detected);
    assert_eq!(result.vendor, "hamamatsu-ndpi");
}
