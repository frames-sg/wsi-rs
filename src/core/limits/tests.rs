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

#[test]
fn every_slide_limit_builder_accepts_positive_values_and_rejects_zero() {
    let limits = SlideLimits::default()
        .with_aggregate_metadata_bytes(1)
        .unwrap()
        .with_metadata_value_bytes(2)
        .unwrap()
        .with_tile_index_bytes(3)
        .unwrap()
        .with_encoded_unit_bytes(4)
        .unwrap()
        .with_decoded_output_bytes(5)
        .unwrap()
        .with_region_pixels(6)
        .unwrap()
        .with_region_rgba_bytes(7)
        .unwrap()
        .with_operation_transient_bytes(8)
        .unwrap()
        .with_slide_transient_bytes(9)
        .unwrap()
        .with_batch_chunk_bytes(10)
        .unwrap();
    assert_eq!(limits.aggregate_metadata_bytes(), 1);
    assert_eq!(limits.metadata_value_bytes(), 2);
    assert_eq!(limits.tile_index_bytes(), 3);
    assert_eq!(limits.encoded_unit_bytes(), 4);
    assert_eq!(limits.decoded_output_bytes(), 5);
    assert_eq!(limits.region_pixels(), 6);
    assert_eq!(limits.region_rgba_bytes(), 7);
    assert_eq!(limits.operation_transient_bytes(), 8);
    assert_eq!(limits.slide_transient_bytes(), 9);
    assert_eq!(limits.batch_chunk_bytes(), 10);

    for error in [
        SlideLimits::default().with_aggregate_metadata_bytes(0),
        SlideLimits::default().with_decoded_output_bytes(0),
        SlideLimits::default().with_region_pixels(0),
        SlideLimits::default().with_region_rgba_bytes(0),
        SlideLimits::default().with_slide_transient_bytes(0),
        SlideLimits::default().with_batch_chunk_bytes(0),
    ] {
        assert!(error.unwrap_err().to_string().contains("greater than zero"));
    }
}

#[test]
fn admission_recovers_from_a_poisoned_state_mutex() {
    let admission = SlideAdmission::new(16);
    let poisoner = Arc::clone(&admission);
    assert!(std::thread::spawn(move || {
        let _state = poisoner.state.lock().unwrap();
        panic!("poison admission state");
    })
    .join()
    .is_err());

    let reservation = admission.reserve(8, None).unwrap();
    drop(reservation);
    assert_eq!(
        admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .in_flight,
        0
    );
}
