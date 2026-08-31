use super::*;

#[test]
fn read_associated_composites_tiled_ifd_images() {
    let tiles = [vec![10u8; 4], vec![20u8; 4], vec![30u8; 4], vec![40u8; 4]];
    let file = build_tiled_associated_tiff(4, 4, 2, 2, &tiles);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = associated_image_layout(
        DatasetId::new(1),
        "label",
        (4, 4),
        1,
        TileSource::TiledIfd {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::None,
        },
    );
    let reader = TiffPixelReader::new(container, layout);

    let image = reader.read_associated("label").unwrap();
    let rgb = image.data.as_u8().unwrap();
    let expected = vec![
        10, 10, 10, 10, 10, 10, 20, 20, 20, 20, 20, 20, 10, 10, 10, 10, 10, 10, 20, 20, 20, 20, 20,
        20, 30, 30, 30, 30, 30, 30, 40, 40, 40, 40, 40, 40, 30, 30, 30, 30, 30, 30, 40, 40, 40, 40,
        40, 40,
    ];
    assert_eq!(rgb, expected.as_slice());
    let pixel = |x: usize, y: usize| -> [u8; 3] {
        let idx = (y * image.width as usize + x) * 3;
        [rgb[idx], rgb[idx + 1], rgb[idx + 2]]
    };

    assert_eq!(pixel(0, 0), [10, 10, 10]);
    assert_eq!(pixel(3, 0), [20, 20, 20]);
    assert_eq!(pixel(0, 3), [30, 30, 30]);
    assert_eq!(pixel(3, 3), [40, 40, 40]);
}

#[test]
fn read_associated_thumbnail_assembly_matches_expected_rgb_bytes_with_edge_tiles() {
    let tiles = [
        vec![10u8; 4],
        vec![20u8; 4],
        vec![30u8, 0, 30, 0],
        vec![40u8, 40, 0, 0],
        vec![50u8, 50, 0, 0],
        vec![60u8, 0, 0, 0],
    ];
    let file = build_tiled_associated_tiff(5, 3, 2, 2, &tiles);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = associated_image_layout(
        DatasetId::new(1),
        "label",
        (5, 3),
        1,
        TileSource::TiledIfd {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::None,
        },
    );
    let reader = TiffPixelReader::new(container, layout);

    let image = reader.read_associated("label").unwrap();
    let rgb = image.data.as_u8().unwrap();
    let grayscale_pixels = [10u8, 10, 20, 20, 30, 10, 10, 20, 20, 30, 40, 40, 50, 50, 60];
    let expected: Vec<u8> = grayscale_pixels
        .into_iter()
        .flat_map(|value| [value, value, value])
        .collect();

    assert_eq!(image.width, 5);
    assert_eq!(image.height, 3);
    assert_eq!(rgb, expected.as_slice());
}

#[test]
fn read_associated_composes_multi_strip_jpeg_image() {
    let width = 4;
    let height = 4;
    let rows_per_strip = 2;

    let mut top = image::RgbImage::new(width, rows_per_strip);
    for pixel in top.pixels_mut() {
        *pixel = image::Rgb([220, 40, 10]);
    }
    let mut bottom = image::RgbImage::new(width, rows_per_strip);
    for pixel in bottom.pixels_mut() {
        *pixel = image::Rgb([15, 80, 210]);
    }

    let encode_strip = |img: &image::RgbImage| {
        let mut encoded = Vec::new();
        JpegEncoder::new(&mut encoded, 100)
            .encode(
                img.as_raw().as_slice(),
                img.width() as u16,
                img.height() as u16,
                JpegColorType::Rgb,
            )
            .unwrap();
        encoded
    };
    let file = build_multi_stripped_jpeg_tiff(
        width,
        height,
        rows_per_strip,
        &[encode_strip(&top), encode_strip(&bottom)],
    );
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let strip_offsets = container
        .get_u64_array(ifd_id, tags::STRIP_OFFSETS)
        .unwrap();
    let strip_byte_counts = container
        .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)
        .unwrap();
    let layout = associated_image_layout(
        DatasetId::new(17),
        "label",
        (width, height),
        3,
        TileSource::Stripped {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::Jpeg,
            strip_offsets: strip_offsets.to_vec(),
            strip_byte_counts: strip_byte_counts.to_vec(),
        },
    );
    let reader = TiffPixelReader::new(container, layout);

    let image = reader.read_associated("label").unwrap();
    let rgb = image.data.as_u8().unwrap();
    let pixel = |x: usize, y: usize| -> [u8; 3] {
        let idx = (y * image.width as usize + x) * 3;
        [rgb[idx], rgb[idx + 1], rgb[idx + 2]]
    };

    let top_left = pixel(0, 0);
    let top_right = pixel((width - 1) as usize, 0);
    let bottom_left = pixel(0, 3);
    let bottom_right = pixel((width - 1) as usize, 3);
    let strip_delta = |a: [u8; 3], b: [u8; 3]| -> u16 {
        a.into_iter()
            .zip(b)
            .map(|(lhs, rhs)| lhs.abs_diff(rhs) as u16)
            .sum()
    };

    assert!(strip_delta(top_left, top_right) < 20);
    assert!(strip_delta(bottom_left, bottom_right) < 20);
    assert!(strip_delta(top_left, bottom_left) > 80);
}

#[test]
fn read_associated_decodes_single_strip_jpeg_image() {
    let width = 4;
    let height = 3;
    let expected = [45u8, 125, 215];
    let mut source = image::RgbImage::new(width, height);
    for pixel in source.pixels_mut() {
        *pixel = image::Rgb(expected);
    }

    let mut jpeg = Vec::new();
    JpegEncoder::new(&mut jpeg, 100)
        .encode(
            source.as_raw().as_slice(),
            source.width() as u16,
            source.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    let file = build_stripped_jpeg_tiff(width, height, &jpeg);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = associated_image_layout(
        DatasetId::new(18),
        "thumbnail",
        (width, height),
        3,
        stripped_associated_source(&container, ifd_id, Compression::Jpeg),
    );
    let reader = TiffPixelReader::new(container, layout);

    let image = reader.read_associated("thumbnail").unwrap();
    assert_eq!(image.width, width);
    assert_eq!(image.height, height);
    assert_eq!(image.channels, 3);
    assert_eq!(image.color_space, ColorSpace::Rgb);
    let rgb = image.data.as_u8().unwrap();
    assert_eq!(rgb.len(), width as usize * height as usize * 3);
    for pixel in rgb.chunks_exact(3) {
        let delta: u16 = pixel
            .iter()
            .copied()
            .zip(expected)
            .map(|(actual, want)| actual.abs_diff(want) as u16)
            .sum();
        assert!(delta < 12, "unexpected decoded pixel {pixel:?}");
    }
}

#[test]
fn read_associated_uncompressed_single_sample_rgb_photometric_treated_as_grayscale() {
    let pixels = [12u8, 34, 56, 78, 90, 123, 150, 210];
    let file = build_stripped_uncompressed_tiff(4, 2, &pixels, 1, Some(2));
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = associated_image_layout(
        DatasetId::new(23),
        "thumbnail",
        (4, 2),
        1,
        stripped_associated_source(&container, ifd_id, Compression::None),
    );
    let reader = TiffPixelReader::new(container, layout);

    let image = reader.read_associated("thumbnail").unwrap();
    assert_eq!(image.width, 4);
    assert_eq!(image.height, 2);
    assert_eq!(image.channels, 1);
    assert_eq!(image.color_space, ColorSpace::Grayscale);
    assert_eq!(image.data.as_u8().unwrap(), pixels.as_slice());
}

#[test]
fn tiff_predictor_reconstructs_8bit_horizontal_deltas() {
    let encoded = [10u8, 5, 5, 1, 2, 3];
    let file = build_stripped_uncompressed_tiff_with_predictor(3, 2, &encoded, 1, Some(1), Some(2));
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = DatasetLayout {
        dataset: Dataset {
            id: DatasetId::new(24),
            scenes: vec![],
            associated_images: HashMap::new(),
            properties: Properties::new(),
            icc_profiles: HashMap::new(),
            source_icc_profiles: Vec::new(),
        },
        tile_sources: HashMap::new(),
        associated_sources: HashMap::new(),
    };
    let reader = TiffPixelReader::new(container, layout);
    let mut data = encoded.to_vec();

    reader
        .apply_tiff_predictor(ifd_id, 3, 2, &mut data)
        .unwrap();

    assert_eq!(data, [10, 15, 20, 1, 3, 6]);
}

#[test]
fn read_associated_deflate_predictor_uses_tilecodec_path() {
    let expected = [10u8, 15, 20, 1, 3, 6];
    let predictor_encoded = [10u8, 5, 5, 1, 2, 3];
    let mut encoder = ZlibEncoder::new(Vec::new(), DeflateCompression::fast());
    encoder.write_all(&predictor_encoded).unwrap();
    let compressed = encoder.finish().unwrap();
    let file = build_stripped_tiff(3, 2, &compressed, 1, Some(1), Some(2), 8);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = associated_image_layout(
        DatasetId::new(25),
        "thumbnail",
        (3, 2),
        1,
        stripped_associated_source(&container, ifd_id, Compression::Deflate),
    );
    let reader = TiffPixelReader::new(container, layout);

    let image = reader.read_associated("thumbnail").unwrap();

    assert_eq!(image.data.as_u8().unwrap(), expected.as_slice());
}

#[test]
fn read_associated_decompresses_each_deflate_strip_independently() {
    let width = 3;
    let height = 4;
    let rows_per_strip = 2;
    let raw_strips = [vec![1u8, 2, 3, 4, 5, 6], vec![7u8, 8, 9, 10, 11, 12]];
    let strips = raw_strips
        .iter()
        .map(|raw| {
            let mut encoder = ZlibEncoder::new(Vec::new(), DeflateCompression::fast());
            encoder.write_all(raw).unwrap();
            encoder.finish().unwrap()
        })
        .collect::<Vec<_>>();
    let file = build_multi_stripped_tiff(width, height, rows_per_strip, &strips, 8, 1, 1);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let layout = associated_image_layout(
        DatasetId::new(26),
        "thumbnail",
        (width, height),
        1,
        TileSource::Stripped {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::Deflate,
            strip_offsets: container
                .get_u64_array(ifd_id, tags::STRIP_OFFSETS)
                .unwrap()
                .to_vec(),
            strip_byte_counts: container
                .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)
                .unwrap()
                .to_vec(),
        },
    );
    let reader = TiffPixelReader::new(container, layout);

    let image = reader.read_associated("thumbnail").unwrap();

    assert_eq!(image.data.as_u8().unwrap(), raw_strips.concat().as_slice());
}

#[test]
fn stripped_compressed_data_validates_metadata_and_zero_fills_missing_strips() {
    let width = 3;
    let height = 4;
    let rows_per_strip = 2;
    let file =
        build_multi_stripped_tiff(width, height, rows_per_strip, &[vec![1], vec![2]], 8, 1, 1);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let layout = associated_image_layout(
        DatasetId::new(28),
        "thumbnail",
        (width, height),
        1,
        TileSource::Stripped {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::Deflate,
            strip_offsets: container
                .get_u64_array(ifd_id, tags::STRIP_OFFSETS)
                .unwrap()
                .to_vec(),
            strip_byte_counts: container
                .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)
                .unwrap()
                .to_vec(),
        },
    );
    let reader = TiffPixelReader::new(container, layout);

    let mismatched = reader
        .read_stripped_compressed_data(
            "thumbnail",
            ifd_id,
            Compression::Deflate,
            (width, height),
            &[0],
            &[],
        )
        .unwrap_err();
    assert!(mismatched.to_string().contains("mismatched strip metadata"));

    let missing = reader
        .read_stripped_compressed_data(
            "thumbnail",
            ifd_id,
            Compression::Deflate,
            (width, height),
            &[0],
            &[0],
        )
        .unwrap_err();
    assert!(missing.to_string().contains("expected at least 2 strips"));

    let oversized = reader
        .read_stripped_compressed_data(
            "thumbnail",
            ifd_id,
            Compression::Deflate,
            (width, height),
            &[0, 0],
            &[MAX_COMPRESSED_INPUT_BYTES, 1],
        )
        .unwrap_err();
    assert!(oversized.to_string().contains("compressed strips exceed"));

    let decoded = reader
        .read_stripped_compressed_data(
            "thumbnail",
            ifd_id,
            Compression::Deflate,
            (width, height),
            &[0, 0],
            &[0, 0],
        )
        .unwrap();
    assert_eq!(decoded, vec![0; (width * height) as usize]);
}

#[test]
fn read_associated_decodes_tiff_flavored_lzw() {
    let expected = (0u8..=255).cycle().take(256 * 32).collect::<Vec<_>>();
    let mut encoder = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
    let compressed = encoder.encode(&expected).unwrap();
    let file = build_stripped_tiff(256, 32, &compressed, 1, Some(1), None, 5);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let layout = associated_image_layout(
        DatasetId::new(27),
        "label",
        (256, 32),
        1,
        stripped_associated_source(&container, ifd_id, Compression::Lzw),
    );
    let reader = TiffPixelReader::new(container, layout);

    let image = reader.read_associated("label").unwrap();

    assert_eq!(image.data.as_u8().unwrap(), expected.as_slice());
}

fn build_single_tile_jp2k_layout(
    container: Arc<TiffContainer>,
    compression: Compression,
    width: u32,
    height: u32,
) -> TiffPixelReader {
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = associated_image_layout(
        DatasetId::new(1),
        "label",
        (width, height),
        3,
        TileSource::TiledIfd {
            ifd_id,
            jpeg_tables: None,
            compression,
        },
    );
    TiffPixelReader::new(container, layout)
}

fn assert_sample_buffer_matches_rgb_fixture(image: &CpuTile, expected_rgb: &image::RgbImage) {
    assert_cpu_tile_matches_rgb_fixture_with_tolerance(
        image,
        expected_rgb,
        50,
        1600,
        "JP2K tiled decode",
    );
}

#[test]
fn read_associated_decodes_jp2k_rgb_tile_from_tiled_ifd() {
    let codestream = include_bytes!("../../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let expected = load_fixture_rgb(include_bytes!(
        "../../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));
    let file = build_tiled_associated_tiff(
        expected.width(),
        expected.height(),
        expected.width(),
        expected.height(),
        &[codestream],
    );
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let reader = build_single_tile_jp2k_layout(
        container,
        Compression::Jp2kRgb,
        expected.width(),
        expected.height(),
    );

    let image = reader.read_associated("label").unwrap();
    assert_sample_buffer_matches_rgb_fixture(&image, &expected);
}

#[test]
fn read_associated_decodes_jp2k_ycbcr_tile_from_tiled_ifd() {
    let codestream = include_bytes!("../../../../../tests/fixtures/jp2k/ycbcr_420.j2k").to_vec();
    let expected = load_fixture_rgb(include_bytes!(
        "../../../../../tests/fixtures/jp2k/ycbcr_420.ppm"
    ));
    let file = build_tiled_associated_tiff(
        expected.width(),
        expected.height(),
        expected.width(),
        expected.height(),
        &[codestream],
    );
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let reader = build_single_tile_jp2k_layout(
        container,
        Compression::Jp2kYcbcr,
        expected.width(),
        expected.height(),
    );

    let image = reader.read_associated("label").unwrap();
    assert_sample_buffer_matches_rgb_fixture(&image, &expected);
}
