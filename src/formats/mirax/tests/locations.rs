use super::super::*;
use super::fixtures::MiraxFixture;
use crate::core::registry::Slide;

fn stored_unit(slide: &Slide, req: &TileRequest) -> (StoredTile, EncodedUnit) {
    let TileLocation::Stored(stored) = slide.tile_location(req).expect("MIRAX tile location")
    else {
        panic!("MIRAX tile {req:?} must be stored");
    };
    let unit = stored
        .single_unit()
        .expect("MIRAX tiles come from one record")
        .clone();
    (stored, unit)
}

fn segment_bytes(unit: &EncodedUnit) -> Vec<Vec<u8>> {
    let file = std::fs::read(unit.path()).expect("read MIRAX data file");
    unit.segments()
        .iter()
        .map(|range| file[range.offset as usize..range.end().unwrap() as usize].to_vec())
        .collect()
}

fn assembled(unit: &EncodedUnit) -> Vec<u8> {
    unit.assemble(segment_bytes(unit).iter().map(Vec::as_slice))
        .expect("assemble MIRAX record")
}

#[test]
fn whole_jpeg_tile_location_round_trips_the_raw_tile() {
    let fixture = MiraxFixture::complete();
    let slide = Slide::open(&fixture.path).unwrap();
    let req = TileRequest::new(0, 0, 0, 1, 2);
    let (stored, unit) = stored_unit(&slide, &req);

    assert_eq!(stored.tile_size(), (16, 16));
    assert_eq!(unit.path().file_name().unwrap(), "Data0000.dat");
    assert_eq!(unit.framing(), StoredFraming::Standalone);
    assert_eq!(unit.codec().compression(), Compression::Jpeg);
    assert_eq!(
        (
            unit.codec().bits_per_sample(),
            unit.codec().samples_per_pixel()
        ),
        (8, 3)
    );
    assert_eq!(unit.stored_size(), (16, 16));
    assert_eq!(unit.source_region(), PixelRect::new(0, 0, 16, 16));
    assert_eq!(unit.tile_region(), PixelRect::new(0, 0, 16, 16));

    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(assembled(&unit), raw.data());
    assert_eq!(raw.compression(), Compression::Jpeg);
    let decoded = crate::decode::jpeg::decode_standalone_jpeg(raw.data()).unwrap();
    assert_eq!(decoded.as_u8(), slide.read_tile(&req).unwrap().as_u8());
}

#[test]
fn cropped_png_and_bmp_tiles_locate_their_source_regions() {
    let fixture = MiraxFixture::complete();
    let slide = Slide::open(&fixture.path).unwrap();
    for (req, source) in [
        (TileRequest::new(0, 0, 1, 1, 0), PixelRect::new(8, 0, 8, 8)),
        (TileRequest::new(0, 0, 1, 2, 3), PixelRect::new(0, 8, 8, 8)),
        (TileRequest::new(0, 0, 2, 2, 1), PixelRect::new(8, 4, 4, 4)),
    ] {
        let (stored, unit) = stored_unit(&slide, &req);
        assert_eq!(stored.tile_size(), (source.width, source.height));
        // complete() stores level 1 as PNG and level 2 as BMP.
        let compression = match req.level.get() {
            1 => Compression::Png,
            _ => Compression::Bmp,
        };
        assert_eq!(unit.codec().compression(), compression);
        assert_eq!(unit.stored_size(), (16, 16));
        assert_eq!(unit.source_region(), source);
        assert_eq!(
            unit.tile_region(),
            PixelRect::new(0, 0, source.width, source.height)
        );

        // The record is a complete image file; crop it to get the tile.
        let record = image::load_from_memory(&assembled(&unit))
            .unwrap()
            .into_rgb8();
        let crop =
            image::imageops::crop_imm(&record, source.x, source.y, source.width, source.height)
                .to_image();
        let tile = slide.read_tile(&req).unwrap();
        assert_eq!(tile.as_u8(), Some(crop.as_raw().as_slice()));

        let raw = slide.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(raw.compression(), Compression::None);
        assert_eq!(
            (raw.width(), raw.height(), raw.samples_per_pixel()),
            (source.width, source.height, 3)
        );
        assert_eq!(Some(raw.data()), tile.as_u8());
    }
}

#[test]
fn whole_png_and_bmp_records_pass_through_unchanged() {
    for format in ["PNG", "BMP24"] {
        let fixture = MiraxFixture::with_formats([format, "PNG", "BMP24"]);
        let slide = Slide::open(&fixture.path).unwrap();
        let req = TileRequest::new(0, 0, 0, 2, 1);
        let (_, unit) = stored_unit(&slide, &req);
        assert_eq!(unit.source_region(), PixelRect::new(0, 0, 16, 16));

        let raw = slide.read_raw_compressed_tile(&req).unwrap();
        let compression = if format == "PNG" {
            Compression::Png
        } else {
            Compression::Bmp
        };
        assert_eq!(raw.compression(), compression);
        assert_eq!(
            (raw.width(), raw.height(), raw.bits_allocated()),
            (16, 16, 8)
        );
        assert_eq!(assembled(&unit), raw.data());
        let decoded = image::load_from_memory(raw.data()).unwrap().into_rgb8();
        assert_eq!(
            slide.read_tile(&req).unwrap().as_u8(),
            Some(decoded.as_raw().as_slice())
        );
    }
}

#[test]
fn cropped_jpeg_tiles_are_cut_losslessly_from_the_stored_jpeg() {
    use j2k_jpeg::transcode::{extract_dct_blocks, DctExtractOptions};
    let fixture = MiraxFixture::with_formats(["JPEG", "JPEG", "JPEG"]);
    let slide = Slide::open(&fixture.path).unwrap();
    for (req, source, region) in [
        // An MCU-aligned crop is exactly the tile.
        (
            TileRequest::new(0, 0, 1, 1, 1),
            PixelRect::new(8, 8, 8, 8),
            PixelRect::new(0, 0, 8, 8),
        ),
        // Otherwise the crop starts at the previous MCU boundary.
        (
            TileRequest::new(0, 0, 2, 1, 2),
            PixelRect::new(4, 8, 4, 4),
            PixelRect::new(4, 0, 4, 4),
        ),
    ] {
        let (_, unit) = stored_unit(&slide, &req);
        assert_eq!(unit.codec().compression(), Compression::Jpeg);
        assert_eq!(unit.source_region(), source);

        let raw = slide.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(raw.compression(), Compression::Jpeg);
        assert_eq!(raw.tile_region(), region);
        assert_eq!(
            (raw.width(), raw.height()),
            (region.x + region.width, region.y + region.height)
        );

        // The fixture is 4:4:4, so the crop decodes to the tile's pixels.
        let decoded = crate::decode::jpeg::decode_standalone_jpeg(raw.data()).unwrap();
        let decoded = decoded.as_u8().unwrap();
        let expected: Vec<u8> = (region.y..region.y + region.height)
            .flat_map(|y| {
                let start = ((y * raw.width() + region.x) * 3) as usize;
                decoded[start..start + region.width as usize * 3].to_vec()
            })
            .collect();
        assert_eq!(
            slide.read_tile(&req).unwrap().as_u8(),
            Some(expected.as_slice())
        );

        // Quantized coefficients are copied from the stored JPEG unchanged.
        let stored = extract_dct_blocks(&assembled(&unit), DctExtractOptions::default()).unwrap();
        let cropped = extract_dct_blocks(raw.data(), DctExtractOptions::default()).unwrap();
        let (block_x, block_y) = (
            ((source.x - region.x) / 8) as usize,
            ((source.y - region.y) / 8) as usize,
        );
        for (src, out) in stored.components.iter().zip(&cropped.components) {
            for by in 0..out.block_rows as usize {
                for bx in 0..out.block_cols as usize {
                    assert_eq!(
                        out.quantized_blocks[by * out.block_cols as usize + bx],
                        src.quantized_blocks
                            [(by + block_y) * src.block_cols as usize + bx + block_x]
                    );
                }
            }
        }
    }
}

#[test]
fn level_locations_list_stored_entries_and_sparse_tiles_are_absent() {
    let fixture = MiraxFixture::complete();
    let slide = Slide::open(&fixture.path).unwrap();
    for level in 0..3u32 {
        let locations = slide
            .level_tile_locations(0, 0, level, PlaneSelection::default())
            .unwrap();
        assert_eq!(locations.tiles.len(), 15, "level {level}");
        assert!(locations
            .tiles
            .iter()
            .all(|tile| matches!(tile.location, TileLocation::Stored(_))));
        assert!(!locations
            .tiles
            .iter()
            .any(|tile| (tile.col, tile.row) == (3, 3)));
        assert_eq!(
            slide
                .tile_location(&TileRequest::new(0, 0, level, 3, 3))
                .unwrap(),
            TileLocation::Absent
        );
    }
}

#[test]
fn fractional_subtiles_are_not_addressable() {
    let fixture = MiraxFixture::complete();
    fixture.write_slidedat(
        &fixture
            .complete_slidedat()
            .replacen("DIGITIZER_WIDTH=16", "DIGITIZER_WIDTH=15", 3)
            .replacen("DIGITIZER_HEIGHT=16", "DIGITIZER_HEIGHT=15", 3),
    );
    let slide = Slide::open(&fixture.path).unwrap();
    let req = TileRequest::new(0, 0, 1, 1, 0);
    assert_eq!(
        slide.tile_location(&req).unwrap(),
        TileLocation::NotAddressable(NotAddressableReason::Resampled)
    );
    let error = slide.read_raw_compressed_tile(&req).unwrap_err();
    assert!(error.to_string().contains("Resampled"), "{error}");
}

#[test]
fn record_past_the_data_file_end_is_an_error() {
    let fixture = MiraxFixture::complete();
    let mut index = fixture.read_index();
    let read_u32 = |bytes: &[u8], offset: usize| {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
    };
    let hierarchy_table = read_u32(&index, 14);
    let level0_head = read_u32(&index, hierarchy_table);
    let level0_page = read_u32(&index, level0_head + 4);
    // First level-0 record: image index, offset, length, file number.
    index[level0_page + 16..level0_page + 20].copy_from_slice(&i32::MAX.to_le_bytes());
    fixture.write_index(&index);

    let slide = Slide::open(&fixture.path).unwrap();
    let error = slide
        .tile_location(&TileRequest::new(0, 0, 0, 0, 0))
        .expect_err("a record past EOF has no valid location");
    assert!(error.to_string().contains("exceeds"), "{error}");
    assert!(slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .is_err());
}

/// Appends `record` to the data file and points the first level-1 record at
/// it, leaving the declared image size unchanged.
fn replace_first_level1_record(fixture: &MiraxFixture, record: &[u8]) {
    let mut data = std::fs::read(&fixture.data_path).unwrap();
    let offset = i32::try_from(data.len()).unwrap();
    data.extend_from_slice(record);
    super::fixtures::write_bytes(&fixture.data_path, &data);
    let mut index = fixture.read_index();
    let read_u32 = |bytes: &[u8], offset: usize| {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
    };
    let hierarchy_table = read_u32(&index, 14);
    let level1_head = read_u32(&index, hierarchy_table + 4);
    let level1_page = read_u32(&index, level1_head + 4);
    // First level-1 record: image index, offset, length, file number.
    index[level1_page + 12..level1_page + 16].copy_from_slice(&offset.to_le_bytes());
    index[level1_page + 16..level1_page + 20]
        .copy_from_slice(&i32::try_from(record.len()).unwrap().to_le_bytes());
    fixture.write_index(&index);
}

#[test]
fn crop_of_a_declared_size_jpeg_with_large_marker_segments_fits_its_reservation() {
    let fixture = MiraxFixture::with_formats(["JPEG", "JPEG", "JPEG"]);
    // Three maximum-length COM segments make the record far larger than its
    // 16x16 image's coefficients; the reservation must still cover the crop.
    let jpeg = super::fixtures::encode_jpeg(16, 16, 7);
    let mut padded = jpeg[..2].to_vec();
    for _ in 0..3 {
        padded.extend_from_slice(&[0xFF, 0xFE, 0xFF, 0xFF]);
        padded.extend(std::iter::repeat_n(b'x', 65533));
    }
    padded.extend_from_slice(&jpeg[2..]);
    replace_first_level1_record(&fixture, &padded);
    let slide = Slide::open(&fixture.path).unwrap();

    let raw = slide
        .read_raw_compressed_tile(&TileRequest::new(0, 0, 1, 1, 1))
        .unwrap();
    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!(raw.tile_region().width, 8);
}

#[test]
fn crop_of_a_jpeg_larger_than_declared_returns_the_whole_record() {
    let fixture = MiraxFixture::with_formats(["JPEG", "JPEG", "JPEG"]);
    // Point the first level-1 record at a 512x512 JPEG while Slidedat.ini
    // still declares 16x16 images, so its coefficients outgrow the bytes
    // admission reserves for the tile.
    let large = super::fixtures::encode_jpeg(512, 512, 7);
    replace_first_level1_record(&fixture, &large);

    let slide = Slide::open(&fixture.path).unwrap();
    let req = TileRequest::new(0, 0, 1, 1, 1);
    let (_, unit) = stored_unit(&slide, &req);
    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!(raw.data(), large.as_slice());
    assert_eq!(raw.data(), assembled(&unit));
    assert_eq!((raw.width(), raw.height()), (512, 512));
    assert_eq!(raw.tile_region(), unit.source_region());

    // A crop of a record of the declared size still fits its reservation.
    let raw = slide
        .read_raw_compressed_tile(&TileRequest::new(0, 0, 1, 3, 1))
        .unwrap();
    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!((raw.width(), raw.height()), (8, 8));
}

#[test]
fn jpeg_crops_over_the_lossless_crop_limit_return_the_whole_record() {
    let fixture = MiraxFixture::with_formats(["JPEG", "JPEG", "JPEG"]);
    let limits = crate::SlideLimits::default()
        .with_lossless_crop_bytes(1024)
        .unwrap();
    let slide = Slide::open_with_options(
        &fixture.path,
        crate::SlideOpenOptions::default().with_limits(limits),
    )
    .unwrap();
    let req = TileRequest::new(0, 0, 2, 1, 2);
    let (_, unit) = stored_unit(&slide, &req);
    assert_eq!(unit.source_region(), PixelRect::new(4, 8, 4, 4));

    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!(raw.data(), assembled(&unit));
    assert_eq!((raw.width(), raw.height()), (16, 16));
    assert_eq!(raw.tile_region(), unit.source_region());
    // The record decodes to the tile at `tile_region`.
    let decoded = crate::decode::jpeg::decode_standalone_jpeg(raw.data()).unwrap();
    let decoded = decoded.as_u8().unwrap();
    let region = raw.tile_region();
    let expected: Vec<u8> = (region.y..region.y + region.height)
        .flat_map(|y| {
            let start = ((y * 16 + region.x) * 3) as usize;
            decoded[start..start + region.width as usize * 3].to_vec()
        })
        .collect();
    assert_eq!(
        slide.read_tile(&req).unwrap().as_u8(),
        Some(expected.as_slice())
    );

    // Under the default limit the same tile is cropped, with the same color.
    let crop = Slide::open(&fixture.path)
        .unwrap()
        .read_raw_compressed_tile(&req)
        .unwrap();
    assert_eq!((crop.width(), crop.height()), (8, 4));
    assert_eq!(
        crop.photometric_interpretation(),
        raw.photometric_interpretation()
    );
}

#[test]
fn only_raw_reads_of_jpeg_crop_tiles_reserve_the_crop_working_set() {
    let fixture = MiraxFixture::with_formats(["JPEG", "JPEG", "JPEG"]);
    let reader = MiraxReader {
        slide: Arc::new(MiraxSlide::parse(&fixture.path).unwrap()),
    };
    let crop = TileRequest::new(0, 0, 1, 1, 1);
    let (entry, tile) = reader.tile_for_request(&crop).unwrap();
    let reservation = super::super::locations::jpeg_crop_reservation(entry, tile).unwrap();
    let decode = reader.tile_encoded_upper_bound(&crop).unwrap();
    assert!(decode < reservation, "{decode} < {reservation}");
    assert_eq!(
        reader.raw_tile_encoded_upper_bound(&crop).unwrap(),
        reservation
    );
    assert_eq!(
        reader
            .tile_batch_encoded_upper_bound(std::slice::from_ref(&crop))
            .unwrap(),
        decode
    );

    // A raw read of a whole-record tile holds the record and a possible
    // EOI-repaired copy, never the decoded pixels a decode reserves, so
    // large stored JPEGs stay within the operation limit.
    let whole = TileRequest::new(0, 0, 0, 1, 1);
    let (_, tile) = reader.tile_for_request(&whole).unwrap();
    let record = tile.image.record.len * 2 + 2;
    assert_eq!(reader.raw_tile_encoded_upper_bound(&whole).unwrap(), record);
    assert!(record < reader.tile_encoded_upper_bound(&whole).unwrap());
}
