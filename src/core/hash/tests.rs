use super::*;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn dataset_id_uses_first_128_quickhash_bits() {
    let path = Path::new("slide.svs");
    let id = dataset_id_from_quickhash(
        path,
        "00112233445566778899aabbccddeefffedcba9876543210",
        "quickhash",
    )
    .expect("valid quickhash prefix");

    assert_eq!(id, DatasetId::new(0x00112233445566778899aabbccddeeff));
}

#[test]
fn dataset_id_preserves_path_and_hash_label_in_errors() {
    let path = Path::new("fixtures/broken.zvi");

    assert!(matches!(
        dataset_id_from_quickhash(path, "abcd", "ZVI quickhash"),
        Err(WsiError::InvalidSlide { path: error_path, message })
            if error_path == path && message == "ZVI quickhash too short"
    ));
    assert!(matches!(
        dataset_id_from_quickhash(
            path,
            "gggggggggggggggggggggggggggggggg",
            "ZVI quickhash",
        ),
        Err(WsiError::InvalidSlide { path: error_path, message })
            if error_path == path && message == "ZVI quickhash is not valid hex"
    ));
}

#[test]
fn hash_data_produces_hex_string() {
    let mut h = Quickhash1::new();
    h.update(b"hello world");
    let result = h.finish().unwrap();
    // SHA-256 hex is always 64 characters
    assert_eq!(result.len(), 64);
    // Verify it's valid hex
    assert!(result.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn hash_string_includes_null_terminator() {
    // hash_string("abc") should equal update(b"abc\0")
    let mut h1 = Quickhash1::new();
    h1.hash_string("abc");
    let r1 = h1.finish().unwrap();

    let mut h2 = Quickhash1::new();
    h2.update(b"abc\0");
    let r2 = h2.finish().unwrap();

    assert_eq!(r1, r2);
}

#[test]
fn disabled_hash_returns_none() {
    let mut h = Quickhash1::new();
    h.update(b"data");
    h.disable();
    assert!(h.finish().is_none());
}

#[test]
fn hash_file_part() {
    // Write "0123456789" to temp file, hash bytes 2..7 ("23456")
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"0123456789").unwrap();
    tmp.flush().unwrap();

    let mut h1 = Quickhash1::new();
    h1.hash_file_part(tmp.path(), 2, Some(5)).unwrap();
    let r1 = h1.finish().unwrap();

    // Compare with direct hash of "23456"
    let mut h2 = Quickhash1::new();
    h2.update(b"23456");
    let r2 = h2.finish().unwrap();

    assert_eq!(r1, r2);
}

#[test]
fn hash_file_part_offset_past_eof_errors() {
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"0123456789").unwrap();
    tmp.flush().unwrap();

    let mut h = Quickhash1::new();
    let err = h.hash_file_part(tmp.path(), 20, Some(1)).unwrap_err();
    assert!(err.to_string().contains("offset 20 exceeds file length 10"));
}

#[test]
fn hash_file_part_range_past_eof_errors() {
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"0123456789").unwrap();
    tmp.flush().unwrap();

    let mut h = Quickhash1::new();
    let err = h.hash_file_part(tmp.path(), 8, Some(5)).unwrap_err();
    assert!(err.to_string().contains("only 2 bytes remain"));
}
