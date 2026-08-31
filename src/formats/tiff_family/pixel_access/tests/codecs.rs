use super::*;

fn jpeg_sof(ids: [u8; 3], sampling: [(u8, u8); 3]) -> Vec<u8> {
    let mut jpeg = vec![
        0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x01, 0x00, 0x01, 0x03,
    ];
    for idx in 0..3 {
        jpeg.push(ids[idx]);
        jpeg.push((sampling[idx].0 << 4) | sampling[idx].1);
        jpeg.push(0);
    }
    jpeg
}

#[test]
fn jpeg_rgb_component_ids_zero_one_two_follow_tiff_photometric() {
    let jpeg = jpeg_sof([0, 1, 2], [(1, 1), (1, 1), (1, 1)]);

    assert_eq!(
        jpeg_bitstream_color_hint(&jpeg, None),
        JpegBitstreamColorHint::RgbComponentIds012
    );
    assert_eq!(
        tiff_jpeg_color_transform(2, 3, jpeg_bitstream_color_hint(&jpeg, None)),
        J2kColorTransform::ForceRgb
    );
    assert_eq!(
        tiff_jpeg_color_transform(6, 3, jpeg_bitstream_color_hint(&jpeg, None)),
        J2kColorTransform::ForceYCbCr
    );
}

#[test]
fn jpeg_rgb_component_ids_ascii_force_rgb() {
    let jpeg = jpeg_sof([b'R', b'G', b'B'], [(1, 1), (1, 1), (1, 1)]);

    assert_eq!(
        jpeg_bitstream_color_hint(&jpeg, None),
        JpegBitstreamColorHint::Rgb
    );
    assert_eq!(
        tiff_jpeg_color_transform(6, 3, jpeg_bitstream_color_hint(&jpeg, None)),
        J2kColorTransform::ForceRgb
    );
}

#[test]
fn jpeg_rgb_tiff_with_actual_chroma_subsampling_uses_ycbcr_hint() {
    let jpeg = jpeg_sof([1, 2, 3], [(2, 2), (1, 1), (1, 1)]);

    assert_eq!(
        jpeg_bitstream_color_hint(&jpeg, None),
        JpegBitstreamColorHint::YCbCr
    );
    assert_eq!(
        tiff_jpeg_color_transform(2, 3, jpeg_bitstream_color_hint(&jpeg, None)),
        J2kColorTransform::ForceYCbCr
    );
}

#[test]
fn jpeg_unknown_bitstream_falls_back_to_tiff_photometric() {
    assert_eq!(
        tiff_jpeg_color_transform(2, 3, JpegBitstreamColorHint::Unknown),
        J2kColorTransform::ForceRgb
    );
    assert_eq!(
        tiff_jpeg_color_transform(6, 3, JpegBitstreamColorHint::Unknown),
        J2kColorTransform::ForceYCbCr
    );
}

// ── FullDecodeCache tests ─────────────────────────────────────

#[test]
fn raw_compressed_tile_returns_standalone_tiled_jpeg_byte_identical() {
    let jpeg = encode_solid_rgb_jpeg(8, 8, [200, 10, 30]);
    let reader = build_tiled_jpeg_reader(8, 8, 8, 8, std::slice::from_ref(&jpeg));

    let raw = reader
        .read_raw_compressed_tile(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();

    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!((raw.width(), raw.height()), (8, 8));
    assert_eq!(raw.bits_allocated(), 8);
    assert_eq!(raw.samples_per_pixel(), 3);
    assert_eq!(raw.data(), jpeg);
}

#[test]
fn standalone_jpeg_frame_owned_keeps_allocation_when_tables_are_embedded() {
    let jpeg = encode_solid_rgb_jpeg(8, 8, [90, 40, 210]);
    let input_ptr = jpeg.as_ptr();

    let (frame, info) = standalone_jpeg_frame_owned(jpeg, None).unwrap();

    assert_eq!(frame.as_ptr(), input_ptr);
    assert_eq!((info.width, info.height), (8, 8));
    assert_eq!(info.bits_allocated, 8);
    assert_eq!(info.samples_per_pixel, 3);
}

#[test]
fn raw_compressed_tile_rebuilds_tiled_jpeg_with_jpeg_tables_without_reencoding_entropy() {
    let jpeg = encode_solid_rgb_jpeg(8, 8, [40, 180, 90]);
    let (abbreviated_tile, jpeg_tables) = split_test_jpeg_tables(&jpeg);
    let reader = build_tiled_jpeg_reader_with_tables(
        8,
        8,
        8,
        8,
        std::slice::from_ref(&abbreviated_tile),
        jpeg_tables,
    );

    let raw = reader
        .read_raw_compressed_tile(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();

    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!((raw.width(), raw.height()), (8, 8));
    assert!(raw.data().len() > abbreviated_tile.len());
    assert!(raw.data().windows(2).any(|bytes| bytes == [0xFF, 0xDB]));
    assert!(raw.data().windows(2).any(|bytes| bytes == [0xFF, 0xC4]));
    assert!(raw.data().ends_with(&[0xFF, 0xD9]));
    assert!(raw
        .data()
        .windows(abbreviated_tile.len().saturating_sub(2))
        .any(|window| window == &abbreviated_tile[2..]));
}

#[test]
fn raw_compressed_tile_returns_tiled_jp2k_rgb_byte_identical() {
    let codestream = include_bytes!("../../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let expected = load_fixture_rgb(include_bytes!(
        "../../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));
    let reader = build_tiled_encoded_reader(
        expected.width(),
        expected.height(),
        expected.width(),
        expected.height(),
        std::slice::from_ref(&codestream),
        Compression::Jp2kRgb,
        33004,
        3,
        2,
    );

    let raw = reader
        .read_raw_compressed_tile(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();

    assert_eq!(raw.compression(), Compression::Jp2kRgb);
    assert_eq!(
        (raw.width(), raw.height()),
        (expected.width(), expected.height())
    );
    assert_eq!(raw.bits_allocated(), 8);
    assert_eq!(raw.samples_per_pixel(), 3);
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Rgb
    );
    assert_eq!(raw.data(), codestream);
}

#[test]
fn raw_compressed_tile_returns_standalone_ndpi_restart_jpeg() {
    let (reader, _) = build_test_ndpi_reader_for_strip_cache(128, 16, 1);

    let raw = reader
        .read_raw_compressed_tile(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 1u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();

    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!((raw.width(), raw.height()), (128, 16));
    assert_eq!(raw.bits_allocated(), 8);
    assert_eq!(raw.samples_per_pixel(), 3);
    assert!(raw.data().starts_with(&[0xFF, 0xD8]));
    assert!(raw.data().ends_with(&[0xFF, 0xD9]));
    assert!(raw.data().windows(2).any(|bytes| bytes == [0xFF, 0xC0]));
    assert!(raw.data().windows(2).any(|bytes| bytes == [0xFF, 0xDA]));

    let decoded = decode_jpeg_rgb_with_size_override(
        raw.data(),
        None,
        raw.width(),
        raw.height(),
        None,
        None,
        J2kColorTransform::Auto,
    )
    .expect("decode raw NDPI JPEG tile");
    assert_eq!((decoded.width, decoded.height), (128, 16));
}

#[test]
fn raw_compressed_tile_rejects_ndpi_restart_segments_that_cross_rows() {
    let (reader, _) = build_test_ndpi_reader_for_strip_cache(130, 16, 2);

    let err = reader
        .read_raw_compressed_tile(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 1u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap_err();

    assert!(
        err.to_string().contains("align to image rows"),
        "unexpected error: {err}"
    );
}

#[test]
fn empty_rgb_tile_rejects_overflowing_dimensions() {
    let err = match TiffPixelReader::empty_rgb_tile(u32::MAX, u32::MAX) {
        Ok(_) => panic!("overflowing empty RGB tile should be rejected"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("overflow output buffer size"),
        "unexpected error: {err}"
    );
}

#[test]
fn tile_codec_kind_classifies_tiff_jpeg_and_jp2k_sources() {
    let jpeg_tiles = [encode_solid_rgb_jpeg(8, 8, [200, 10, 10])];
    let jpeg_reader = build_tiled_jpeg_reader(8, 8, 8, 8, &jpeg_tiles);
    let req = TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    assert_eq!(jpeg_reader.tile_codec_kind(&req), TileCodecKind::Jpeg);

    let codestream = include_bytes!("../../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let expected = load_fixture_rgb(include_bytes!(
        "../../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));
    let jp2k_reader = build_tiled_encoded_reader(
        expected.width(),
        expected.height(),
        expected.width(),
        expected.height(),
        &[codestream],
        Compression::Jp2kRgb,
        33004,
        3,
        2,
    );
    assert_eq!(jp2k_reader.tile_codec_kind(&req), TileCodecKind::Jp2k);
}
