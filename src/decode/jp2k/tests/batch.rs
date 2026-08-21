use super::*;

#[test]
fn tile_batch_decodes_in_submission_order_with_cpu_fallback_policy() {
    let first_codestream = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_420.j2k");
    let first_header = parse_codestream_header(first_codestream).unwrap();
    let first_expected = load_fixture_rgb(include_bytes!(
        "../../../../tests/fixtures/jp2k/ycbcr_420.ppm"
    ));
    let second_codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let second_header = parse_codestream_header(second_codestream).unwrap();
    let second_expected = load_fixture_rgb(include_bytes!(
        "../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));

    let requests = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(first_codestream),
            expected_width: first_header.image_width,
            expected_height: first_header.image_height,
            rgb_color_space: false,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(second_codestream),
            expected_width: second_header.image_width,
            expected_height: second_header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = decode_jp2k_tile_batch_to_sample_buffers(&requests).unwrap();

    assert_eq!(decoded.len(), 2);
    assert_sample_buffer_matches_rgb_fixture(&decoded[0], &first_expected);
    assert_sample_buffer_matches_rgb_fixture(&decoded[1], &second_expected);
}

#[test]
fn rgb_tile_batch_j2k_helper_decodes_in_submission_order() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let expected = load_fixture_rgb(include_bytes!(
        "../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));

    let requests = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = decode_jp2k_tile_batch_with_j2k(&requests).unwrap();

    assert_eq!(decoded.len(), 2);
    assert_sample_buffer_matches_rgb_fixture(&decoded[0], &expected);
    assert_sample_buffer_matches_rgb_fixture(&decoded[1], &expected);
}

#[test]
fn j2k_cpu_batch_fast_path_decodes_in_submission_order() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let expected = load_fixture_rgb(include_bytes!(
        "../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));
    let requests = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = try_decode_batch_jp2k_with_j2k(&requests)
        .expect("valid CPU JP2K jobs should take the j2k batch fast path");

    assert_eq!(decoded.len(), 2);
    assert_sample_buffer_matches_rgb_fixture(&decoded[0], &expected);
    assert_sample_buffer_matches_rgb_fixture(&decoded[1], &expected);
}

#[test]
fn materialize_jp2k_batch_outputs_preserves_order_and_converts_ycbcr() {
    let prepared = vec![
        PreparedJp2kBatchJob {
            decoded_width: 1,
            decoded_height: 1,
            expected_width: 1,
            expected_height: 1,
            output_colorspace: Jp2kColorSpace::YCbCr,
        },
        PreparedJp2kBatchJob {
            decoded_width: 1,
            decoded_height: 1,
            expected_width: 1,
            expected_height: 1,
            output_colorspace: Jp2kColorSpace::Rgb,
        },
    ];
    let outputs = vec![vec![128, 128, 128], vec![10, 20, 30]];
    let decoded = materialize_jp2k_batch_outputs(prepared, outputs).unwrap();

    assert_eq!(decoded[0].data.as_u8().unwrap(), &[128, 128, 128]);
    assert_eq!(decoded[1].data.as_u8().unwrap(), &[10, 20, 30]);
}

#[test]
fn decode_batch_jp2k_preserves_order_and_per_tile_results() {
    let first_codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let first_header = parse_codestream_header(first_codestream).unwrap();
    let second_codestream = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_420.j2k");
    let second_header = parse_codestream_header(second_codestream).unwrap();
    let jobs = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(first_codestream),
            expected_width: first_header.image_width,
            expected_height: first_header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(second_codestream),
            expected_width: second_header.image_width,
            expected_height: second_header.image_height,
            rgb_color_space: false,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = decode_batch_jp2k(&jobs);

    assert_eq!(decoded.len(), 2);
    assert!(decoded[0].is_ok());
    assert!(decoded[1].is_ok());
    assert_eq!(decoded[0].as_ref().unwrap().width, first_header.image_width);
    assert_eq!(
        decoded[1].as_ref().unwrap().width,
        second_header.image_width
    );
}

#[test]
fn decode_batch_jp2k_reports_malformed_tile_without_losing_good_tiles() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let jobs = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(b"not j2k"),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = decode_batch_jp2k(&jobs);

    assert_eq!(decoded.len(), 2);
    assert!(decoded[0].is_ok());
    assert!(decoded[1].is_err());
}
