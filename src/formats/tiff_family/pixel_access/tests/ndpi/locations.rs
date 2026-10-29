use super::super::locations::{
    assemble_unit, assert_location_round_trip, single_unit, stored_tile, tile_location,
};
use super::super::*;
use super::fixtures::*;

const DRI_DISABLED: [u8; 6] = [0xFF, 0xDD, 0x00, 0x04, 0x00, 0x00];

fn level0_request(col: i64, row: i64) -> TileRequest {
    TileRequest::new(0usize, 0usize, 0u32, col, row)
}

fn assert_restart_tiles_round_trip(reader: &TiffPixelReader) {
    let mut prefixes = Vec::new();
    for row in 0..2 {
        for col in 0..2 {
            let (unit, raw) = assert_location_round_trip(reader, &level0_request(col, row));
            assert_eq!(unit.framing(), StoredFraming::Prefixed);
            assert_eq!(unit.stored_size(), (64, 8));
            assert_eq!(unit.suffix(), [0xFF, 0xD9]);
            assert!(unit.prefix().windows(6).any(|bytes| bytes == DRI_DISABLED));
            let info = parse_baseline_jpeg_frame_info(unit.prefix()).unwrap();
            assert_eq!((info.width, info.height), (64, 8));
            assert_eq!((raw.width(), raw.height()), (64, 8));
            prefixes.push(unit.prefix().as_ptr());

            let decoded = decode_jpeg_rgb_with_size_override(
                raw.data(),
                None,
                64,
                8,
                None,
                None,
                J2kColorTransform::Auto,
            )
            .unwrap();
            let center = (4 * 64 + 32) * 3;
            let expected = TEST_NDPI_RESTART_COLORS[(row * 2 + col) as usize];
            for (actual, expected) in decoded.pixels[center..center + 3].iter().zip(expected) {
                assert!(actual.abs_diff(expected) <= 16, "tile ({col},{row})");
            }
        }
    }
    assert!(
        prefixes.windows(2).all(|pair| pair[0] == pair[1]),
        "tiles of one level share one prefix buffer"
    );
}

#[test]
fn ndpi_restart_locations_assemble_to_the_raw_tiles() {
    // The fixture's MCU-starts table points at each interval's RST marker.
    assert_restart_tiles_round_trip(&build_test_ndpi_restart_reader(false));
}

#[test]
fn ndpi_restart_locations_follow_tables_that_point_past_restart_markers() {
    let mut image = image::RgbImage::new(128, 16);
    for (idx, rgb) in TEST_NDPI_RESTART_COLORS.iter().enumerate() {
        let (x0, y0) = ((idx as u32 % 2) * 64, (idx as u32 / 2) * 8);
        for y in y0..y0 + 8 {
            for x in x0..x0 + 64 {
                image.put_pixel(x, y, image::Rgb(*rgb));
            }
        }
    }
    let jpeg = encode_restart_rgb_jpeg(&image, 95, 8);
    let header_len = find_test_jpeg_bitstream_start(&jpeg).unwrap();
    let mut starts = test_jpeg_restart_segment_starts(&jpeg);
    starts.truncate(4);
    // Real NDPI tables point just past each RST marker.
    for start in &mut starts[1..] {
        *start += 2;
    }
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    buf.extend_from_slice(&le_u32(0));
    buf.extend_from_slice(&jpeg);
    let file = finish_ndpi_mcu_tiff(buf, 4, 128, 16, 8, jpeg.len() as u32, &starts);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let layout = build_test_ndpi_layout_from_header(TestNdpiJpegLayout {
        ifd_id,
        dimensions: (128, 16),
        virtual_tile: (64, 8),
        tile_grid: (2, 2),
        jpeg_header: jpeg[..header_len].to_vec(),
        strip_byte_count: jpeg.len() as u64,
    });

    assert_restart_tiles_round_trip(&TiffPixelReader::new(container, layout));
}

#[test]
fn ndpi_unlocatable_intervals_locate_the_whole_level_jpeg() {
    let (file, jpeg_header, strip_byte_count, strip_offset) =
        build_ndpi_scan_data_tiff_from_blobs_with_mcu_mode_and_offset(
            128,
            16,
            &TEST_NDPI_RESTART_COLORS,
            false,
            TestMcuStartsMode::InvalidFileAbsolute,
        );
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let reader = TiffPixelReader::new(
        container,
        build_test_ndpi_layout_from_header_with_strip_offset(
            TestNdpiJpegLayout {
                ifd_id,
                dimensions: (128, 16),
                virtual_tile: (64, 8),
                tile_grid: (2, 2),
                jpeg_header,
                strip_byte_count,
            },
            strip_offset,
        ),
    );

    let location = tile_location(&reader, &level0_request(1, 1));
    let unit = single_unit(&location);
    assert_eq!(unit.framing(), StoredFraming::Standalone);
    assert_eq!(
        unit.segments(),
        [ByteRange::new(strip_offset, strip_byte_count)]
    );
    assert_eq!(unit.stored_size(), (128, 16));
    assert_eq!(unit.source_region(), PixelRect::new(64, 8, 64, 8));
    assert_eq!(unit.tile_region(), PixelRect::new(0, 0, 64, 8));
    let decoded = decode_jpeg_rgb_with_size_override(
        &assemble_unit(&reader, unit),
        None,
        128,
        16,
        None,
        None,
        J2kColorTransform::Auto,
    )
    .unwrap();
    assert_eq!((decoded.width, decoded.height), (128, 16));
}

#[test]
fn ndpi_intervals_that_cross_rows_locate_the_whole_level_jpeg() {
    let (reader, _) = build_test_ndpi_reader_for_strip_cache(130, 16, 2);
    let req = TileRequest::new(0usize, 0usize, 1u32, 1, 0);

    let location = tile_location(&reader, &req);
    assert_eq!(stored_tile(&location).tile_size(), (2, 16));
    let unit = single_unit(&location);
    assert_eq!(unit.framing(), StoredFraming::Standalone);
    assert_eq!(unit.stored_size(), (130, 16));
    assert_eq!(unit.source_region(), PixelRect::new(128, 0, 2, 16));
}

fn full_decode_reader(zero_sof_dimensions: bool) -> (TiffPixelReader, Vec<u8>, u64) {
    full_decode_reader_with_limits(zero_sof_dimensions, crate::SlideLimits::default())
}

/// A 128 x 16 NDPI level without restart markers, read as 64 x 8 tiles
/// under `limits`.
fn full_decode_reader_with_limits(
    zero_sof_dimensions: bool,
    limits: crate::SlideLimits,
) -> (TiffPixelReader, Vec<u8>, u64) {
    let (file, jpeg_header, strip_byte_count) = build_ndpi_scan_data_tiff_from_blobs(
        128,
        16,
        &TEST_NDPI_RESTART_COLORS,
        zero_sof_dimensions,
    );
    let container = Arc::new(
        TiffContainer::open_with_budget(
            file.path(),
            crate::core::registry::OpenBudget::new(limits),
        )
        .unwrap(),
    );
    let ifd_id = container.top_ifds()[0];
    let layout = single_series_layout(
        DatasetId::new(13),
        vec![whole_level((128, 16), 1.0, (64, 8))],
        HashMap::from([(
            tile_source_key(0),
            TileSource::NdpiFullDecode {
                ifd_id,
                jpeg_header: jpeg_header.clone(),
                strip_offset: 8,
                strip_byte_count,
            },
        )]),
    );
    (
        TiffPixelReader::new(container, layout),
        jpeg_header,
        strip_byte_count,
    )
}

#[test]
fn ndpi_full_decode_tiles_locate_their_rect_of_the_level_jpeg() {
    let (reader, _, strip_byte_count) = full_decode_reader(false);

    for (col, row) in [(0, 0), (1, 1)] {
        let location = tile_location(&reader, &level0_request(col, row));
        let unit = single_unit(&location);
        assert_eq!(unit.framing(), StoredFraming::Standalone);
        assert_eq!(unit.segments(), [ByteRange::new(8, strip_byte_count)]);
        assert_eq!(
            unit.source_region(),
            PixelRect::new(64 * col as u32, 8 * row as u32, 64, 8)
        );
        assert_eq!(unit.tile_region(), PixelRect::new(0, 0, 64, 8));
    }
}

#[test]
fn ndpi_zero_sof_levels_get_a_header_stating_the_level_size() {
    let (reader, jpeg_header, strip_byte_count) = full_decode_reader(true);

    let location = tile_location(&reader, &level0_request(0, 0));
    let unit = single_unit(&location);
    assert_eq!(unit.framing(), StoredFraming::Prefixed);
    assert_eq!(unit.stored_size(), (128, 16));
    let header_len = jpeg_header.len() as u64;
    assert_eq!(
        unit.segments(),
        [ByteRange::new(
            8 + header_len,
            strip_byte_count - header_len
        )]
    );
    let info = parse_baseline_jpeg_frame_info(unit.prefix()).unwrap();
    assert_eq!((info.width, info.height), (128, 16));
    let decoded = decode_jpeg_rgb_with_size_override(
        &assemble_unit(&reader, unit),
        None,
        128,
        16,
        None,
        None,
        J2kColorTransform::Auto,
    )
    .unwrap();
    let pixel = &decoded.pixels[(4 * 128 + 96) * 3..(4 * 128 + 96) * 3 + 3];
    for (actual, expected) in pixel.iter().zip(TEST_NDPI_RESTART_COLORS[1]) {
        assert!(actual.abs_diff(expected) <= 16, "pixel {pixel:?}");
    }
}

#[test]
fn ndpi_full_decode_tiles_are_lossless_crops_of_the_level_jpeg() {
    let (reader, _, _) = full_decode_reader(false);
    let full = reader.read_tile_cpu(&level0_request(0, 0)).unwrap();
    assert_eq!((full.width, full.height), (64, 8));

    for (col, row) in [(0, 0), (1, 1)] {
        let req = level0_request(col, row);
        let raw = reader.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(raw.compression(), Compression::Jpeg);
        assert_eq!((raw.width(), raw.height()), (64, 8));
        assert_eq!(raw.tile_region(), PixelRect::new(0, 0, 64, 8));
        let decoded = decode_jpeg_rgb_with_size_override(
            raw.data(),
            None,
            64,
            8,
            None,
            None,
            J2kColorTransform::Auto,
        )
        .unwrap();
        let tile = reader.read_tile_cpu(&req).unwrap();
        assert_eq!(tile.data.as_u8(), Some(decoded.pixels.as_slice()));
    }
}

#[test]
fn ndpi_level_crops_over_the_lossless_crop_limit_return_the_whole_level_jpeg() {
    use crate::core::registry::ManagedSlideReader;
    let limits = crate::SlideLimits::default()
        .with_lossless_crop_bytes(1)
        .unwrap();
    let req = level0_request(1, 1);
    // Both fixtures store the same pixels; only the first decodes tiles.
    let tile = full_decode_reader(false).0.read_tile_cpu(&req).unwrap();
    for zero_sof_dimensions in [false, true] {
        let (reader, _, _) = full_decode_reader_with_limits(zero_sof_dimensions, limits);
        let location = tile_location(&reader, &req);
        let unit = single_unit(&location);
        assert_eq!(unit.source_region(), PixelRect::new(64, 8, 64, 8));

        let raw = reader.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(raw.compression(), Compression::Jpeg);
        assert_eq!(raw.data(), assemble_unit(&reader, unit));
        assert_eq!((raw.width(), raw.height()), (128, 16));
        assert_eq!(raw.tile_region(), unit.source_region());
        assert!(
            reader.raw_tile_encoded_upper_bound(&req).unwrap() >= raw.data().len() as u64,
            "the raw read reserves the whole level JPEG it returns"
        );
        let decoded = decode_jpeg_rgb_with_size_override(
            raw.data(),
            None,
            128,
            16,
            None,
            None,
            J2kColorTransform::Auto,
        )
        .unwrap();
        let expected: Vec<u8> = (8..16)
            .flat_map(|row| decoded.pixels[(row * 128 + 64) * 3..(row * 128 + 128) * 3].to_vec())
            .collect();
        assert_eq!(
            tile.data.as_u8(),
            Some(expected.as_slice()),
            "zero SOF dimensions: {zero_sof_dimensions}"
        );
    }
}

#[test]
fn ndpi_level_jpegs_over_the_encoded_unit_limit_are_refused() {
    let (_, _, strip_byte_count) = full_decode_reader(false);
    let limits = crate::SlideLimits::default()
        .with_encoded_unit_bytes(strip_byte_count - 1)
        .unwrap();
    let (reader, _, _) = full_decode_reader_with_limits(false, limits);
    let err = reader
        .read_raw_compressed_tile(&level0_request(1, 1))
        .unwrap_err();
    assert!(
        matches!(
            err,
            WsiError::ResourceLimit {
                resource: "encoded tile/frame unit",
                requested,
                ..
            } if requested == strip_byte_count
        ),
        "{err:?}"
    );
}

#[test]
fn ndpi_whole_level_tiles_pass_the_level_jpeg_through() {
    let (file, jpeg_header, strip_byte_count) =
        build_ndpi_scan_data_tiff_from_blobs(128, 16, &TEST_NDPI_RESTART_COLORS, false);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let layout = single_series_layout(
        DatasetId::new(14),
        vec![whole_level((128, 16), 1.0, (128, 16))],
        HashMap::from([(
            tile_source_key(0),
            TileSource::NdpiFullDecode {
                ifd_id,
                jpeg_header,
                strip_offset: 8,
                strip_byte_count,
            },
        )]),
    );
    let reader = TiffPixelReader::new(container, layout);

    let (unit, raw) = assert_location_round_trip(&reader, &level0_request(0, 0));
    assert_eq!(unit.segments(), [ByteRange::new(8, strip_byte_count)]);
    assert_eq!((raw.width(), raw.height()), (128, 16));
}

#[test]
fn ndpi_zero_sof_tiles_crop_the_framed_level_jpeg() {
    let (reader, _, _) = full_decode_reader(true);
    let raw = reader
        .read_raw_compressed_tile(&level0_request(1, 0))
        .unwrap();
    assert_eq!((raw.width(), raw.height()), (64, 8));
    let decoded = decode_jpeg_rgb_with_size_override(
        raw.data(),
        None,
        64,
        8,
        None,
        None,
        J2kColorTransform::Auto,
    )
    .unwrap();
    let center = (4 * 64 + 32) * 3;
    for (actual, expected) in decoded.pixels[center..center + 3]
        .iter()
        .zip(TEST_NDPI_RESTART_COLORS[1])
    {
        assert!(actual.abs_diff(expected) <= 16);
    }
}

#[test]
fn ndpi_adobe_rgb_headers_locate_and_read_rgb_tiles() {
    // Numeric component ids with an Adobe APP14 transform 0: the decoder
    // reads the samples as untransformed RGB, whatever the SOF sampling.
    let mut image = image::RgbImage::new(128, 16);
    for (idx, rgb) in TEST_NDPI_RESTART_COLORS.iter().enumerate() {
        let (x0, y0) = ((idx as u32 % 2) * 64, (idx as u32 / 2) * 8);
        for y in y0..y0 + 8 {
            for x in x0..x0 + 64 {
                image.put_pixel(x, y, image::Rgb(*rgb));
            }
        }
    }
    let mut jpeg = crate::test_support::jpeg_without_jfif(&encode_restart_rgb_jpeg(&image, 95, 8));
    jpeg.splice(
        2..2,
        [
            0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 0,
        ],
    );
    let header_len = find_test_jpeg_bitstream_start(&jpeg).unwrap();
    let jpeg_header = jpeg[..header_len].to_vec();
    assert_eq!(
        parse_baseline_jpeg_frame_info(&jpeg_header)
            .unwrap()
            .photometric_interpretation,
        EncodedTilePhotometricInterpretation::YbrFull422,
        "the SOF alone does not say RGB"
    );
    let mut starts = test_jpeg_restart_segment_starts(&jpeg);
    starts.truncate(4);
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    buf.extend_from_slice(&le_u32(0));
    buf.extend_from_slice(&jpeg);
    let file = finish_ndpi_mcu_tiff(buf, 4, 128, 16, 8, jpeg.len() as u32, &starts);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let restart_reader = TiffPixelReader::new(
        container.clone(),
        build_test_ndpi_layout_from_header(TestNdpiJpegLayout {
            ifd_id,
            dimensions: (128, 16),
            virtual_tile: (64, 8),
            tile_grid: (2, 2),
            jpeg_header: jpeg_header.clone(),
            strip_byte_count: jpeg.len() as u64,
        }),
    );
    assert_eq!(
        restart_reader
            .tiff_jpeg_decode_options_for_data(ifd_id, false, &jpeg_header, None)
            .color_transform,
        J2kColorTransform::ForceRgb
    );
    let full_decode_reader = TiffPixelReader::new(
        container,
        single_series_layout(
            DatasetId::new(15),
            vec![whole_level((128, 16), 1.0, (64, 8))],
            HashMap::from([(
                tile_source_key(0),
                TileSource::NdpiFullDecode {
                    ifd_id,
                    jpeg_header,
                    strip_offset: 8,
                    strip_byte_count: jpeg.len() as u64,
                },
            )]),
        ),
    );

    for (reader, framing) in [
        (&restart_reader, StoredFraming::Prefixed),
        (&full_decode_reader, StoredFraming::Standalone),
    ] {
        for (col, row) in [(0, 0), (1, 1)] {
            let req = level0_request(col, row);
            let unit = single_unit(&tile_location(reader, &req)).clone();
            assert_eq!(unit.framing(), framing);
            assert_eq!(
                unit.codec().photometric_interpretation(),
                EncodedTilePhotometricInterpretation::Rgb,
                "{framing:?} tile ({col},{row}) location"
            );
            let raw = reader.read_raw_compressed_tile(&req).unwrap();
            assert_eq!(
                raw.photometric_interpretation(),
                EncodedTilePhotometricInterpretation::Rgb,
                "{framing:?} tile ({col},{row}) raw"
            );
        }
    }
}
