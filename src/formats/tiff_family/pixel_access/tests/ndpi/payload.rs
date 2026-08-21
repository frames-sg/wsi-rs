use super::super::*;
use super::fixtures::*;

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
                .values()
                .next()
                .cloned()
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
