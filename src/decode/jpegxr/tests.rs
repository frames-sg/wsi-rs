use super::*;

const RGB: &[u8] = include_bytes!("../../../tests/fixtures/jxr/rgb.jxr");

#[test]
fn decoder_rejects_container_geometry_and_sample_mismatches() {
    for (width, height, sample, channels) in [
        (15, 16, SampleType::Uint8, 3),
        (16, 15, SampleType::Uint8, 3),
        (16, 16, SampleType::Uint16, 3),
        (16, 16, SampleType::Float32, 3),
        (16, 16, SampleType::Uint8, 1),
        (16, 16, SampleType::Uint8, 4),
    ] {
        assert!(
            decode_jpegxr(RGB, width, height, sample, channels, SlideLimits::default()).is_err()
        );
    }
}

#[test]
fn decoder_obeys_encoded_output_and_transient_limits() {
    for limits in [
        SlideLimits::default().with_encoded_unit_bytes(1).unwrap(),
        SlideLimits::default().with_decoded_output_bytes(1).unwrap(),
        SlideLimits::default()
            .with_operation_transient_bytes(1)
            .unwrap(),
    ] {
        assert!(decode_jpegxr(RGB, 16, 16, SampleType::Uint8, 3, limits).is_err());
    }
}

#[test]
fn malformed_input_returns_a_codec_error() {
    assert!(matches!(
        decode_jpegxr(
            b"not an image",
            16,
            16,
            SampleType::Uint8,
            3,
            SlideLimits::default()
        ),
        Err(WsiError::Codec {
            codec: "jpeg-xr",
            ..
        })
    ));
}
