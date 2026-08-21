use super::*;

#[test]
fn checked_product_rejects_overflow_and_limit() {
    assert!(checked_product_to_usize(&[u64::MAX, 2], u64::MAX, "image").is_err());
    assert!(checked_product_to_usize(&[8, 8, 3], 100, "image").is_err());
    assert_eq!(
        checked_product_to_usize(&[8, 8, 3], 192, "image").unwrap(),
        192
    );
}

#[test]
fn bounded_read_rejects_one_byte_over_limit() {
    assert_eq!(
        read_to_end_bounded(&b"1234"[..], 4, "input").unwrap(),
        b"1234"
    );
    assert!(read_to_end_bounded(&b"12345"[..], 4, "input").is_err());
}

#[test]
fn bounded_file_read_uses_the_open_handle_and_rejects_oversize_input() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("input.bin");
    std::fs::write(&path, b"12345").expect("write bounded-read fixture");

    assert_eq!(read_file_bounded(&path, 5, "input").unwrap(), b"12345");
    assert_eq!(
        read_file_bounded(&path, 4, "input")
            .expect_err("one byte over the limit must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
}
