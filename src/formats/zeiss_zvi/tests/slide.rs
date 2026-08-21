use super::super::model::ZviImageHeader;
use super::*;

fn header(compression: ZviCompression) -> ZviImageHeader {
    ZviImageHeader {
        width: 2,
        height: 3,
        bytes_per_sample: 1,
        payload_offset: 10,
        compression,
        z: 0,
        c: 0,
        t: 0,
        tile_index: 0,
    }
}

#[test]
fn payload_bounds_reject_offset_trailing_raw_data_and_oversize() {
    let path = Path::new("plane.zvi");
    assert_eq!(
        validated_payload_length(path, 16, &header(ZviCompression::Raw)).unwrap(),
        6
    );
    assert!(validated_payload_length(path, 9, &header(ZviCompression::Jpeg)).is_err());
    assert!(validated_payload_length(path, 17, &header(ZviCompression::Raw)).is_err());
    assert!(validated_payload_length(
        path,
        11 + crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
        &header(ZviCompression::Zlib),
    )
    .is_err());

    let mut oversized_decoded = header(ZviCompression::Zlib);
    oversized_decoded.width = u32::MAX;
    oversized_decoded.height = u32::MAX;
    assert!(validated_payload_length(path, 11, &oversized_decoded).is_err());
}
