use super::super::*;
use super::fixtures::*;

fn build_ndpi_mcu_word_tiff(low: &[u32], high: &[u32]) -> NamedTempFile {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&le_u32(0));

    let low_offset = append_optional_u32_array(&mut buf, low);
    let high_offset = append_optional_u32_array(&mut buf, high);
    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&le_u32(ifd_offset));
    append_ifd_tags(
        &mut buf,
        vec![
            (256, 4, 1, le_u32(1)),
            (257, 4, 1, le_u32(1)),
            (
                65426,
                4,
                low.len() as u32,
                u32_array_offset_or_inline_value(low, low_offset),
            ),
            (
                65432,
                4,
                high.len() as u32,
                u32_array_offset_or_inline_value(high, high_offset),
            ),
        ],
    );
    temp_tiff_from_buffer(&buf)
}

#[test]
fn ndpi_mcu_starts_combine_low_and_high_words() {
    let file = build_ndpi_mcu_word_tiff(&[5, 7], &[1, 2]);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let reader = TiffPixelReader::new(
        Arc::clone(&container),
        single_series_layout(DatasetId::new(1), vec![], HashMap::new()),
    );

    let starts = reader
        .ndpi_mcu_starts(ifd_id, 65426, 8, u64::MAX)
        .expect("combine NDPI MCU-start words");
    assert_eq!(starts.as_slice(), &[0x1_0000_0005, 0x2_0000_0007]);

    let mismatch = build_ndpi_mcu_word_tiff(&[5, 7], &[1]);
    let container = Arc::new(TiffContainer::open(mismatch.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let reader = TiffPixelReader::new(
        Arc::clone(&container),
        single_series_layout(DatasetId::new(2), vec![], HashMap::new()),
    );
    let error = reader
        .ndpi_mcu_starts(ifd_id, 65426, 8, u64::MAX)
        .expect_err("mismatched high-word count must fail");
    assert!(error.to_string().contains("high MCU-start count"));
}

#[test]
fn ndpi_jpeg_tile_payload_rejects_malformed_strip_metadata() {
    let (file, jpeg_header, strip_byte_count) = build_ndpi_scan_data_tiff_from_blobs(
        128,
        16,
        &[[240, 20, 20], [20, 220, 20], [20, 20, 230], [220, 220, 30]],
        false,
    );
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = build_test_ndpi_layout_from_header(TestNdpiJpegLayout {
        ifd_id,
        dimensions: (128, 16),
        virtual_tile: (64, 8),
        tile_grid: (2, 2),
        jpeg_header: jpeg_header.clone(),
        strip_byte_count,
    });
    let reader = TiffPixelReader::new(container, layout);
    let req = TileRequest::new(0usize, 0usize, 0u32, 0, 0);

    let err = reader
        .ndpi_jpeg_tile_payload(
            &req,
            ifd_id,
            &jpeg_header,
            65426,
            2,
            2,
            8,
            strip_byte_count,
            NdpiStripKey {
                ifd_id,
                col: 0,
                native_row: 2,
            },
            64,
            8,
            128,
            16,
        )
        .err()
        .expect("strip row outside the NDPI grid should be rejected");
    assert!(err.to_string().contains("strip row 2 out of range"));

    let err = reader
        .ndpi_jpeg_tile_payload(
            &req,
            ifd_id,
            &jpeg_header,
            65426,
            2,
            2,
            8,
            strip_byte_count,
            NdpiStripKey {
                ifd_id,
                col: 2,
                native_row: 0,
            },
            64,
            8,
            128,
            16,
        )
        .err()
        .expect("strip column outside the NDPI grid should be rejected");
    assert!(err.to_string().contains("strip column 2 out of range"));

    let err = reader
        .ndpi_jpeg_tile_payload(
            &req,
            ifd_id,
            &jpeg_header,
            65426,
            2,
            10,
            8,
            strip_byte_count,
            NdpiStripKey {
                ifd_id,
                col: 0,
                native_row: 3,
            },
            64,
            8,
            128,
            16,
        )
        .err()
        .expect("MCU-starts table lookup outside the payload should be rejected");
    assert!(err.to_string().contains("MCU-starts index"));

    let err = reader
        .ndpi_jpeg_tile_payload(
            &req,
            ifd_id,
            &jpeg_header,
            65426,
            2,
            2,
            8,
            0,
            NdpiStripKey {
                ifd_id,
                col: 0,
                native_row: 0,
            },
            64,
            8,
            128,
            16,
        )
        .err()
        .expect("NDPI segment outside the strip byte count should be rejected");
    assert!(err.to_string().contains("exceeds strip byte count 0"));

    let err = reader
        .ndpi_jpeg_tile_payload(
            &req,
            ifd_id,
            &[],
            65426,
            2,
            2,
            8,
            strip_byte_count,
            NdpiStripKey {
                ifd_id,
                col: 0,
                native_row: 0,
            },
            64,
            8,
            128,
            16,
        )
        .err()
        .expect("empty NDPI JPEG header should be rejected");
    assert!(err.to_string().contains("JPEG header is empty"));
}

#[test]
fn ndpi_jpeg_tile_payload_accepts_relative_and_file_absolute_mcu_starts() {
    let colors = [[240, 20, 20], [20, 220, 20], [20, 20, 230], [220, 220, 30]];
    for mode in [TestMcuStartsMode::Relative, TestMcuStartsMode::FileAbsolute] {
        let (file, jpeg_header, strip_byte_count, strip_offset) =
            build_ndpi_scan_data_tiff_from_blobs_with_mcu_mode_and_offset(
                128, 16, &colors, false, mode,
            );
        let container = Arc::new(TiffContainer::open(file.path()).unwrap());
        let ifd_id = *container.top_ifds().first().unwrap();
        let layout = build_test_ndpi_layout_from_header(TestNdpiJpegLayout {
            ifd_id,
            dimensions: (128, 16),
            virtual_tile: (64, 8),
            tile_grid: (2, 2),
            jpeg_header: jpeg_header.clone(),
            strip_byte_count,
        });
        let reader = TiffPixelReader::new(container, layout);
        let req = TileRequest::new(0usize, 0usize, 0u32, 0, 0);

        let payload = reader
            .ndpi_jpeg_tile_payload(
                &req,
                ifd_id,
                &jpeg_header,
                65426,
                2,
                2,
                strip_offset,
                strip_byte_count,
                NdpiStripKey {
                    ifd_id,
                    col: 0,
                    native_row: 0,
                },
                64,
                8,
                128,
                16,
            )
            .unwrap();

        assert!(payload.jpeg.starts_with(&[0xFF, 0xD8]));
        if matches!(mode, TestMcuStartsMode::FileAbsolute) {
            let cached = reader
                .ndpi_mcu_starts_cache
                .lock()
                .unwrap()
                .first_value()
                .expect("MCU starts were cached");
            assert!(
                cached.iter().all(|start| *start < strip_byte_count),
                "file-absolute starts should be normalized once when cached"
            );
        }
    }
}

#[test]
fn ndpi_jpeg_tile_payload_rejects_invalid_absolute_mcu_starts() {
    let colors = [[240, 20, 20], [20, 220, 20], [20, 20, 230], [220, 220, 30]];
    let (file, jpeg_header, strip_byte_count, strip_offset) =
        build_ndpi_scan_data_tiff_from_blobs_with_mcu_mode_and_offset(
            128,
            16,
            &colors,
            false,
            TestMcuStartsMode::InvalidFileAbsolute,
        );
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = build_test_ndpi_layout_from_header(TestNdpiJpegLayout {
        ifd_id,
        dimensions: (128, 16),
        virtual_tile: (64, 8),
        tile_grid: (2, 2),
        jpeg_header: jpeg_header.clone(),
        strip_byte_count,
    });
    let reader = TiffPixelReader::new(container, layout);
    let req = TileRequest::new(0usize, 0usize, 0u32, 0, 0);

    let err = match reader.ndpi_jpeg_tile_payload(
        &req,
        ifd_id,
        &jpeg_header,
        65426,
        2,
        2,
        strip_offset,
        strip_byte_count,
        NdpiStripKey {
            ifd_id,
            col: 0,
            native_row: 0,
        },
        64,
        8,
        128,
        16,
    ) {
        Ok(_) => panic!("invalid absolute MCU starts should be rejected"),
        Err(err) => err,
    };

    assert!(err.to_string().contains("exceeds strip byte count"));
}
