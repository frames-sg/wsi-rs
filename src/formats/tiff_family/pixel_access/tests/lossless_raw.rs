use super::locations::{assemble_unit, single_unit, stored_tile, tile_location};
use super::*;
use crate::core::registry::OpenBudget;
use j2k_jpeg::transcode::{extract_dct_blocks, DctExtractOptions};

/// A smooth RGB gradient, so crops have distinct content per block.
fn gradient(width: u32, height: u32) -> image::RgbImage {
    image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([
            (x * 9 + y) as u8,
            (y * 7 + 3 * x) as u8,
            ((x + y) * 5) as u8,
        ])
    })
}

fn encode_jpeg(image: &image::RgbImage, sampling: JpegSamplingFactor) -> Vec<u8> {
    encode_jpeg_with(image, sampling, false)
}

fn encode_jpeg_with(
    image: &image::RgbImage,
    sampling: JpegSamplingFactor,
    progressive: bool,
) -> Vec<u8> {
    let mut encoded = Vec::new();
    let mut encoder = JpegEncoder::new(&mut encoded, 90);
    encoder.set_sampling_factor(sampling);
    encoder.set_progressive(progressive);
    encoder
        .encode(
            image.as_raw(),
            image.width() as u16,
            image.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    encoded
}

/// Decodes a raw JPEG tile the way the TIFF reader decodes `photometric`.
fn decode_raw_jpeg(raw: &RawCompressedTile, photometric: u32) -> Vec<u8> {
    let transform =
        tiff_jpeg_color_transform(photometric, 3, jpeg_bitstream_color_hint(raw.data(), None));
    decode_jpeg_rgb_with_size_override(
        raw.data(),
        None,
        raw.width(),
        raw.height(),
        None,
        None,
        transform,
    )
    .unwrap()
    .pixels
}

/// The rows of `region` cut out of an interleaved RGB image `width` wide.
fn crop_rgb(pixels: &[u8], width: u32, region: PixelRect) -> Vec<u8> {
    let stride = width as usize * 3;
    (region.y..region.y + region.height)
        .flat_map(|row| {
            let start = row as usize * stride + region.x as usize * 3;
            pixels[start..start + region.width as usize * 3].to_vec()
        })
        .collect()
}

/// A 24 x 24 single-tile JPEG TIFF read as a Ventana level of 12 x 12 cells.
fn subtile_reader(photometric: u16) -> TiffPixelReader {
    let jpeg = encode_jpeg(&gradient(24, 24), JpegSamplingFactor::R_4_4_4);
    subtile_reader_for(jpeg, photometric)
}

/// `jpeg` as the one stored 24 x 24 tile of a Ventana level of 12 x 12 cells.
fn subtile_reader_for(jpeg: Vec<u8>, photometric: u16) -> TiffPixelReader {
    let mut reader = build_tiled_encoded_reader(
        24,
        24,
        24,
        24,
        &[jpeg],
        Compression::Jpeg,
        7,
        3,
        photometric,
    );
    let ifd_id = reader.container.top_ifds()[0];
    reader.layout.tile_sources.insert(
        tile_source_key(0),
        TileSource::TiledIfdSubtiles {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::Jpeg,
            subtiles_per_tile: 2,
        },
    );
    reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::Irregular {
        tile_advance: (12.0, 12.0),
        extra_tiles: (0, 0, 0, 0),
        tiles: (0..2)
            .flat_map(|col| {
                (0..2).map(move |row| ((col, row), TileEntry::new((0.0, 0.0), (12, 12))))
            })
            .collect(),
    };
    reader
}

#[test]
fn ventana_jpeg_subtiles_crop_losslessly_and_decode_like_read_tile() {
    for (photometric, expected) in [
        (6, EncodedTilePhotometricInterpretation::YbrFull422),
        // RGB-tagged TIFF JPEG: the crop keeps the TIFF reading, not the
        // bitstream's default YCbCr.
        (2, EncodedTilePhotometricInterpretation::Rgb),
    ] {
        let reader = subtile_reader(photometric);
        for (col, row) in [(0, 0), (1, 1)] {
            let req = TileRequest::new(0usize, 0usize, 0u32, col, row);
            let raw = reader.read_raw_compressed_tile(&req).unwrap();
            assert_eq!(raw.compression(), Compression::Jpeg);
            assert_eq!(raw.photometric_interpretation(), expected);
            // The 12-pixel share starts inside the 8-pixel MCU grid.
            let origin = 12 * col as u32;
            let snapped = origin - origin % 8;
            // The crop runs from the MCU corner to the share's far edges.
            let side = origin + 12 - snapped;
            assert_eq!((raw.width(), raw.height()), (side, side));
            let region = raw.tile_region();
            assert_eq!(
                region,
                PixelRect::new(origin - snapped, origin - snapped, 12, 12)
            );

            let decoded = decode_raw_jpeg(&raw, u32::from(photometric));
            let tile = reader.read_tile_cpu(&req).unwrap();
            assert_eq!(
                tile.data.as_u8(),
                Some(crop_rgb(&decoded, raw.width(), region).as_slice()),
                "cell ({col},{row}) photometric {photometric}"
            );
        }
    }
}

/// `jpeg` as the stored 24 x 24 tile of a Ventana level whose only cell is
/// (1, 1), under `limits`.
fn limited_subtile_reader(jpeg: &[u8], limits: crate::SlideLimits) -> TiffPixelReader {
    let file = build_tiled_encoded_tiff(24, 24, 24, 24, &[jpeg.to_vec()], 7, 3, 6);
    let container =
        Arc::new(TiffContainer::open_with_budget(file.path(), OpenBudget::new(limits)).unwrap());
    let ifd_id = container.top_ifds()[0];
    let mut layout = single_series_layout(
        DatasetId::new(61),
        vec![regular_level(24, 24, 24, 24)],
        HashMap::from([(
            tile_source_key(0),
            TileSource::TiledIfdSubtiles {
                ifd_id,
                jpeg_tables: None,
                compression: Compression::Jpeg,
                subtiles_per_tile: 2,
            },
        )]),
    );
    layout.dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::Irregular {
        tile_advance: (12.0, 12.0),
        extra_tiles: (0, 0, 0, 0),
        tiles: HashMap::from([((1, 1), TileEntry::new((0.0, 0.0), (12, 12)))]),
    };
    TiffPixelReader::new(container, layout)
}

#[test]
fn subtile_crops_over_the_lossless_crop_limit_return_the_whole_stored_jpeg() {
    use crate::core::registry::ManagedSlideReader;
    let jpeg = encode_jpeg(&gradient(24, 24), JpegSamplingFactor::R_4_4_4);
    let req = TileRequest::new(0usize, 0usize, 0u32, 1, 1);
    let limits = crate::SlideLimits::default()
        .with_lossless_crop_bytes(1)
        .unwrap();
    let reader = limited_subtile_reader(&jpeg, limits);

    let location = tile_location(&reader, &req);
    let unit = single_unit(&location);
    assert_eq!(unit.source_region(), PixelRect::new(12, 12, 12, 12));
    let raw = reader.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!(raw.data(), assemble_unit(&reader, unit));
    assert_eq!(raw.data(), jpeg.as_slice());
    assert_eq!((raw.width(), raw.height()), (24, 24));
    assert_eq!(raw.tile_region(), unit.source_region());
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::YbrFull422
    );
    let decoded = decode_raw_jpeg(&raw, 6);
    assert_eq!(
        reader.read_tile_cpu(&req).unwrap().data.as_u8(),
        Some(crop_rgb(&decoded, 24, raw.tile_region()).as_slice())
    );
    // The raw read reserves room for the whole stream it returns; decoded
    // reads keep one encoded unit.
    let reserved = reader.raw_tile_encoded_upper_bound(&req).unwrap();
    assert!(reserved >= raw.data().len() as u64, "{reserved}");
    assert_eq!(
        reader.tile_encoded_upper_bound(&req).unwrap(),
        limits.encoded_unit_bytes()
    );

    // Under the default limit the same cell is cropped, and its raw read
    // reserves the crop working set.
    let reader = limited_subtile_reader(&jpeg, crate::SlideLimits::default());
    let crop = reader.read_raw_compressed_tile(&req).unwrap();
    assert_eq!((crop.width(), crop.height()), (16, 16));
    assert_eq!(
        crop.photometric_interpretation(),
        raw.photometric_interpretation()
    );
    assert_eq!(
        reader.raw_tile_encoded_upper_bound(&req).unwrap(),
        crate::SlideLimits::default().lossless_crop_bytes()
    );
}

#[test]
fn stored_jpegs_over_the_encoded_unit_limit_are_refused_before_reading() {
    let jpeg = encode_jpeg(&gradient(24, 24), JpegSamplingFactor::R_4_4_4);
    let limit = jpeg.len() as u64 - 1;
    let limits = crate::SlideLimits::default()
        .with_encoded_unit_bytes(limit)
        .unwrap()
        .with_lossless_crop_bytes(1)
        .unwrap();
    let err = limited_subtile_reader(&jpeg, limits)
        .read_raw_compressed_tile(&TileRequest::new(0usize, 0usize, 0u32, 1, 1))
        .unwrap_err();
    assert!(
        matches!(
            err,
            WsiError::ResourceLimit {
                resource: "encoded tile/frame unit",
                requested,
                limit: refused,
            } if requested == jpeg.len() as u64 && refused == limit
        ),
        "{err:?}"
    );
}

/// `image` stored as one JPEG strip per `rows_per_strip` rows.
fn jpeg_strips(
    image: &image::RgbImage,
    rows_per_strip: u32,
    sampling: JpegSamplingFactor,
) -> Vec<Vec<u8>> {
    (0..image.height())
        .step_by(rows_per_strip as usize)
        .map(|y| {
            let rows = rows_per_strip.min(image.height() - y);
            let strip = image::imageops::crop_imm(image, 0, y, image.width(), rows).to_image();
            encode_jpeg(&strip, sampling)
        })
        .collect()
}

#[test]
fn jpeg_strip_tiles_crop_the_strips_they_cover() {
    let image = gradient(264, 24);
    let strips = jpeg_strips(&image, 8, JpegSamplingFactor::R_4_4_4);
    let file = build_multi_stripped_jpeg_tiff(264, 24, 8, &strips);
    let slide = crate::Slide::open(file.path()).unwrap();

    for (col, width) in [(0, 256), (1, 8)] {
        let req = TileRequest::new(0usize, 0usize, 0u32, col, 0);
        let location = slide.tile_location(&req).unwrap();
        let TileLocation::Stored(stored) = &location else {
            panic!("strip tile should be stored, got {location:?}");
        };
        assert_eq!(stored.units().len(), 3);
        assert!(stored
            .units()
            .iter()
            .all(|unit| unit.framing() == StoredFraming::Standalone));

        let raw = slide.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(raw.compression(), Compression::Jpeg);
        assert_eq!((raw.width(), raw.height()), (width, 24));
        assert_eq!(raw.tile_region(), PixelRect::new(0, 0, width, 24));
        assert_eq!(
            raw.photometric_interpretation(),
            EncodedTilePhotometricInterpretation::YbrFull422
        );
        let tile = slide.read_tile(&req).unwrap();
        assert_eq!(
            tile.data.as_u8(),
            Some(decode_raw_jpeg(&raw, 6).as_slice()),
            "tile ({col},0)"
        );
    }
}

#[test]
fn subsampled_jpeg_strip_crops_keep_the_source_coefficients() {
    let image = gradient(264, 32);
    let strips = jpeg_strips(&image, 16, JpegSamplingFactor::R_4_2_0);
    let file = build_multi_stripped_jpeg_tiff(264, 32, 16, &strips);
    let slide = crate::Slide::open(file.path()).unwrap();

    let raw = slide
        .read_raw_compressed_tile(&TileRequest::new(0usize, 0usize, 0u32, 1, 0))
        .unwrap();
    assert_eq!((raw.width(), raw.height()), (8, 32));
    let crop = extract_dct_blocks(raw.data(), DctExtractOptions::default()).unwrap();
    let sources: Vec<_> = strips
        .iter()
        .map(|strip| extract_dct_blocks(strip, DctExtractOptions::default()).unwrap())
        .collect();
    for (index, component) in crop.components.iter().enumerate() {
        // The crop starts at pixel column 256: MCU column 16 of 16-pixel MCUs.
        let first_col = 16 * u32::from(component.h_samp);
        for row in 0..component.block_rows {
            let source = &sources[0].components[index];
            let (part, part_row) = (row / source.block_rows, row % source.block_rows);
            let source = &sources[part as usize].components[index];
            for col in 0..component.block_cols {
                assert_eq!(
                    component.quantized_blocks[(row * component.block_cols + col) as usize],
                    source.quantized_blocks
                        [(part_row * source.block_cols + first_col + col) as usize],
                    "component {index} block ({col},{row})"
                );
            }
        }
    }
}

/// A reader of `file`, a strip TIFF, as one strip-based level of `tile`
/// tiles, its strips decoded with `jpeg_tables` under a lossless-crop limit
/// of `lossless_crop_bytes` (the default when `None`).
fn stripped_jpeg_reader(
    file: &NamedTempFile,
    (width, height): (u32, u32),
    tile: (u32, u32),
    rows_per_strip: u32,
    jpeg_tables: Option<Vec<u8>>,
    lossless_crop_bytes: Option<u64>,
) -> TiffPixelReader {
    let limits = match lossless_crop_bytes {
        Some(limit) => crate::SlideLimits::default()
            .with_lossless_crop_bytes(limit)
            .unwrap(),
        None => crate::SlideLimits::default(),
    };
    let container =
        Arc::new(TiffContainer::open_with_budget(file.path(), OpenBudget::new(limits)).unwrap());
    let ifd_id = container.top_ifds()[0];
    let layout = single_series_layout(
        DatasetId::new(62),
        vec![regular_level(width, height, tile.0, tile.1)],
        HashMap::from([(
            tile_source_key(0),
            TileSource::StrippedLevel {
                ifd_id,
                compression: Compression::Jpeg,
                jpeg_tables,
                rows_per_strip,
                strip_offsets: container
                    .get_u64_array(ifd_id, tags::STRIP_OFFSETS)
                    .unwrap()
                    .to_vec(),
                strip_byte_counts: container
                    .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)
                    .unwrap()
                    .to_vec(),
            },
        )]),
    );
    TiffPixelReader::new(container, layout)
}

#[test]
fn whole_jpeg_units_pass_through_unchanged() {
    let image = gradient(64, 32);
    let strips = jpeg_strips(&image, 16, JpegSamplingFactor::R_4_4_4);
    let file = build_multi_stripped_jpeg_tiff(64, 32, 16, &strips);
    let reader = stripped_jpeg_reader(&file, (64, 32), (64, 16), 16, None, None);
    // Each 64 x 16 tile is exactly one strip.
    let req = TileRequest::new(0usize, 0usize, 0u32, 0, 1);

    let location = tile_location(&reader, &req);
    let unit = single_unit(&location);
    let raw = reader.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.data(), strips[1].as_slice());
    assert_eq!(assemble_unit(&reader, unit), raw.data());
}

#[test]
fn whole_progressive_jpeg_units_pass_through_unchanged_with_and_without_tables() {
    let image = gradient(64, 32);
    let strips: Vec<Vec<u8>> = (0..2)
        .map(|strip| {
            let rows = image::imageops::crop_imm(&image, 0, strip * 16, 64, 16).to_image();
            encode_jpeg_with(&rows, JpegSamplingFactor::R_4_2_0, true)
        })
        .collect();
    assert!(
        parse_baseline_jpeg_frame_info(&strips[0]).is_err(),
        "fixture must be progressive"
    );
    let (abbreviated, tables): (Vec<Vec<u8>>, Vec<Vec<u8>>) = strips
        .iter()
        .map(|strip| split_test_jpeg_tables(strip))
        .unzip();

    for (stored, jpeg_tables) in [
        (strips.clone(), None),
        (abbreviated, Some(tables[1].clone())),
    ] {
        let file = build_multi_stripped_jpeg_tiff(64, 32, 16, &stored);
        let reader = stripped_jpeg_reader(&file, (64, 32), (64, 16), 16, jpeg_tables.clone(), None);
        let req = TileRequest::new(0usize, 0usize, 0u32, 0, 1);

        let location = tile_location(&reader, &req);
        let unit = single_unit(&location);
        let raw = reader.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(
            assemble_unit(&reader, unit),
            raw.data(),
            "tables {:?}",
            jpeg_tables.is_some()
        );
        // A whole strip reserves what preparing it holds, not the crop
        // working set.
        let reserved =
            crate::core::registry::ManagedSlideReader::raw_tile_encoded_upper_bound(&reader, &req)
                .unwrap();
        assert!(reserved >= raw.data().len() as u64, "{reserved}");
        assert!(reserved < crate::SlideLimits::default().lossless_crop_bytes());
        assert!(
            parse_baseline_jpeg_frame_info(raw.data()).is_err(),
            "the progressive strip is passed through, not re-encoded"
        );
        assert_eq!((raw.width(), raw.height()), (64, 16));
        assert_eq!(raw.tile_region(), PixelRect::new(0, 0, 64, 16));
        assert_eq!(
            raw.photometric_interpretation(),
            unit.codec().photometric_interpretation()
        );
        if jpeg_tables.is_none() {
            assert_eq!(raw.data(), strips[1].as_slice());
        }
        let decoded = decode_raw_jpeg(&raw, 6);
        assert_eq!(
            reader.read_tile_cpu(&req).unwrap().data.as_u8(),
            Some(decoded.as_slice())
        );
    }
}

#[test]
fn progressive_jpeg_subtiles_crop_to_baseline_tiles() {
    let jpeg = encode_jpeg_with(&gradient(24, 24), JpegSamplingFactor::R_4_4_4, true);
    assert!(
        parse_baseline_jpeg_frame_info(&jpeg).is_err(),
        "fixture must be progressive"
    );
    let reader = subtile_reader_for(jpeg, 6);
    let req = TileRequest::new(0usize, 0usize, 0u32, 1, 1);

    let raw = reader.read_raw_compressed_tile(&req).unwrap();
    assert!(parse_baseline_jpeg_frame_info(raw.data()).is_ok());
    let region = raw.tile_region();
    assert_eq!(region, PixelRect::new(4, 4, 12, 12));
    let decoded = decode_raw_jpeg(&raw, 6);
    assert_eq!(
        reader.read_tile_cpu(&req).unwrap().data.as_u8(),
        Some(crop_rgb(&decoded, raw.width(), region).as_slice())
    );
}

#[test]
fn stored_jpegs_smaller_than_their_tiles_are_refused() {
    // The IFD declares 24 x 24 tiles, but the stored JPEG is 16 x 16, so the
    // share at (12, 12) would come back 4 x 4.
    let jpeg = encode_jpeg(&gradient(16, 16), JpegSamplingFactor::R_4_4_4);
    let reader = subtile_reader_for(jpeg, 6);

    let err = reader
        .read_raw_compressed_tile(&TileRequest::new(0usize, 0usize, 0u32, 1, 1))
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("smaller than its container declares"),
        "unexpected error: {err}"
    );
}

#[test]
fn jpeg_strip_crops_are_refused_from_metadata_before_reading_strips() {
    let strips = jpeg_strips(&gradient(264, 24), 8, JpegSamplingFactor::R_4_4_4);
    let file = build_multi_stripped_jpeg_tiff(264, 24, 8, &strips);
    // Every strip fits the limit on its own; together with the worst-case
    // crop output they do not, and no single stored JPEG holds the tile.
    assert!(strips.iter().all(|strip| strip.len() < 100_000));
    let reader = stripped_jpeg_reader(&file, (264, 24), (256, 256), 8, None, Some(100_000));

    let err = reader
        .read_raw_compressed_tile(&TileRequest::new(0usize, 0usize, 0u32, 1, 0))
        .unwrap_err();
    assert!(
        matches!(
            err,
            WsiError::ResourceLimit {
                resource: "lossless JPEG crop sources and output",
                ..
            }
        ),
        "unexpected error: {err}"
    );
}

#[test]
fn jpeg_strip_stack_crops_over_their_coefficient_budget_are_refused() {
    let strips = jpeg_strips(&gradient(264, 24), 8, JpegSamplingFactor::R_4_4_4);
    let file = build_multi_stripped_jpeg_tiff(264, 24, 8, &strips);
    // The strips and the worst-case output fit, with one byte left for the
    // coefficients once the marker copies are set aside.
    let stored: u64 = strips.iter().map(|strip| strip.len() as u64).sum();
    let output = crate::decode::jpeg::lossless::lossless_crop_output_bound(stored, 256, 24);
    let limit = 2 * stored + output + 1;
    let reader = stripped_jpeg_reader(&file, (264, 24), (256, 256), 8, None, Some(limit));

    let err = reader
        .read_raw_compressed_tile(&TileRequest::new(0usize, 0usize, 0u32, 0, 0))
        .unwrap_err();
    assert!(
        matches!(
            err,
            WsiError::ResourceLimit {
                resource: "lossless JPEG crop DCT coefficients",
                limit: 1,
                ..
            }
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn tiles_inside_one_jpeg_strip_over_the_lossless_crop_limit_return_the_whole_strip() {
    let image = gradient(128, 32);
    let strips = jpeg_strips(&image, 16, JpegSamplingFactor::R_4_4_4);
    let file = build_multi_stripped_jpeg_tiff(128, 32, 16, &strips);
    let reader = stripped_jpeg_reader(&file, (128, 32), (64, 16), 16, None, Some(1));
    // Tile (1, 1) is the right half of the second strip.
    let req = TileRequest::new(0usize, 0usize, 0u32, 1, 1);

    let location = tile_location(&reader, &req);
    let unit = single_unit(&location);
    assert_eq!(unit.source_region(), PixelRect::new(64, 0, 64, 16));
    let raw = reader.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.data(), assemble_unit(&reader, unit));
    assert_eq!(raw.data(), strips[1].as_slice());
    assert_eq!((raw.width(), raw.height()), (128, 16));
    assert_eq!(raw.tile_region(), unit.source_region());
    let decoded = decode_raw_jpeg(&raw, 6);
    assert_eq!(
        reader.read_tile_cpu(&req).unwrap().data.as_u8(),
        Some(crop_rgb(&decoded, 128, raw.tile_region()).as_slice())
    );
}

#[test]
fn jpeg_strip_crop_preflight_counts_the_tables_merged_into_every_strip() {
    // Abbreviated strips share JPEGTables padded with a comment, so merging
    // the tables into each strip, not the strips themselves, is what
    // outgrows the limit.
    let strips = jpeg_strips(&gradient(16, 64), 8, JpegSamplingFactor::R_4_4_4);
    let (abbreviated, tables): (Vec<Vec<u8>>, Vec<Vec<u8>>) = strips
        .iter()
        .map(|strip| split_test_jpeg_tables(strip))
        .unzip();
    let mut padded_tables = tables[0].clone();
    let comment = [0xFF, 0xFE, 0xFF, 0xFF];
    padded_tables.splice(2..2, comment.iter().copied().chain(vec![0; 0xFFFD]));
    let file = build_multi_stripped_jpeg_tiff(16, 64, 8, &abbreviated);
    let stored: u64 = abbreviated.iter().map(|strip| strip.len() as u64).sum();
    let output = crate::decode::jpeg::lossless::lossless_crop_output_bound(stored, 16, 64);
    // The strips and the crop output fit, with room for the tables once.
    let limit = stored + output + padded_tables.len() as u64;
    let req = TileRequest::new(0usize, 0usize, 0u32, 0, 0);

    let reader = stripped_jpeg_reader(
        &file,
        (16, 64),
        (16, 64),
        8,
        Some(padded_tables.clone()),
        Some(limit),
    );
    let location = tile_location(&reader, &req);
    assert_eq!(stored_tile(&location).units().len(), 8);
    let err = reader.read_raw_compressed_tile(&req).unwrap_err();
    let WsiError::ResourceLimit {
        resource,
        requested,
        ..
    } = err
    else {
        panic!("expected a resource limit, got {err}");
    };
    assert_eq!(resource, "lossless JPEG crop sources and output");
    assert!(
        requested >= stored + 8 * padded_tables.len() as u64,
        "requested {requested} must count the tables merged into all 8 strips"
    );

    // With the tables' real size the same crop is admitted and decodes like
    // the strips.
    let reader = stripped_jpeg_reader(
        &file,
        (16, 64),
        (16, 64),
        8,
        Some(tables[0].clone()),
        Some(limit),
    );
    let raw = reader.read_raw_compressed_tile(&req).unwrap();
    let decoded = decode_raw_jpeg(&raw, 6);
    assert_eq!(
        reader.read_tile_cpu(&req).unwrap().data.as_u8(),
        Some(decoded.as_slice())
    );
}
