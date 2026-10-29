use super::fixtures::*;
use super::runtime::{
    dataset_for_levels, reader_and_first_image, test_dicom_image_with_transfer_syntax, tile_request,
};
use super::*;
use crate::core::types::{
    ByteOrder, ByteRange, EncodedUnit, PixelRect, PlanarConfiguration, StoredFraming, TileLocation,
};
use crate::decode::jp2k::{decode_batch_jp2k, Jp2kDecodeJob};

const RGB_CODESTREAM: &[u8] = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");

/// The bytes at each of `unit`'s segments, read from its file.
fn segment_bytes(unit: &EncodedUnit) -> Vec<Vec<u8>> {
    let file = std::fs::read(unit.path()).expect("read located DICOM file");
    unit.segments()
        .iter()
        .map(|range| file[range.offset as usize..][..range.length as usize].to_vec())
        .collect()
}

fn assembled(unit: &EncodedUnit) -> Vec<u8> {
    let bytes = segment_bytes(unit);
    unit.assemble(bytes.iter().map(Vec::as_slice))
        .expect("assemble located frame")
}

fn single_unit(location: TileLocation) -> EncodedUnit {
    match location {
        TileLocation::Stored(stored) => stored.single_unit().expect("one frame").clone(),
        other => panic!("expected a stored frame, got {other:?}"),
    }
}

/// A JPEG with an even byte count, so its DICOM fragment needs no padding.
fn even_length_jpeg(width: u16, height: u16, seed: u8) -> Vec<u8> {
    let mut jpeg = encode_test_jpeg_rgb(width, height, seed);
    if jpeg.len() % 2 == 1 {
        // A one-byte COM segment after SOI flips the parity.
        jpeg.splice(2..2, [0xFF, 0xFE, 0x00, 0x03, 0x00]);
    }
    jpeg
}

/// A 32x24 slide of four 16x16 JPEG frames; the bottom row is cut to 8 rows.
fn write_jpeg_grid(path: &Path) -> Vec<Vec<u8>> {
    let frames = (0..4)
        .map(|seed| even_length_jpeg(16, 16, seed * 40))
        .collect::<Vec<_>>();
    write_test_dicom(
        path,
        TestDicomOptions {
            transfer_syntax: uids::JPEG_BASELINE8_BIT,
            photometric_interpretation: "YBR_FULL_422",
            rows: 16,
            columns: 16,
            total_pixel_matrix_rows: 24,
            total_pixel_matrix_columns: 32,
            number_of_frames: 4,
            pixel_data: TestPixelData::EncapsulatedFrames(frames.clone()),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    frames
}

#[test]
fn jpeg_frame_locations_assemble_to_raw_tiles_that_decode_like_read_tile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-grid.dcm");
    let frames = write_jpeg_grid(&path);
    let slide = Slide::open(&path).unwrap();
    let (_, image) = reader_and_first_image(&path);

    for (index, (col, row)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
        let req = tile_request(col, row);
        let TileLocation::Stored(stored) = slide.tile_location(&req).unwrap() else {
            panic!("JPEG frame ({col}, {row}) is stored");
        };
        let expected_size = if row == 0 { (16, 16) } else { (16, 8) };
        assert_eq!(stored.tile_size(), expected_size);
        let unit = stored.single_unit().expect("one frame per tile");
        assert_eq!(unit.path(), path.as_path());
        assert_eq!(unit.framing(), StoredFraming::Standalone);
        assert_eq!(unit.stored_size(), (16, 16));
        assert_eq!(unit.segments().len(), 1);
        assert_eq!(unit.codec().compression(), Compression::Jpeg);
        assert_eq!(
            unit.codec().photometric_interpretation(),
            EncodedTilePhotometricInterpretation::YbrFull422
        );
        assert_eq!(
            (
                unit.codec().bits_per_sample(),
                unit.codec().samples_per_pixel()
            ),
            (8, 3)
        );

        let bytes = assembled(unit);
        assert_eq!(bytes, frames[index]);
        let raw = slide.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(raw.data(), bytes.as_slice());
        assert_eq!((raw.width(), raw.height()), (16, 16));
        assert_eq!(
            raw.tile_region(),
            PixelRect::new(0, 0, expected_size.0, expected_size.1),
            "edge frames keep their size and locate the tile inside it"
        );

        let decoded = crate::core::batch::exactly_one(
            decode_dicom_jpeg_batch(image.bit_depth, &[image.jpeg_job(&bytes).unwrap()]),
            "located JPEG decode",
        )
        .unwrap()
        .unwrap();
        let decoded =
            crop_or_keep_sample_buffer_rgb(decoded, expected_size.0, expected_size.1).unwrap();
        assert_eq!(
            decoded.data.as_u8(),
            slide.read_tile(&req).unwrap().data.as_u8()
        );
    }
}

#[test]
fn level_tile_locations_list_every_frame_of_the_grid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-grid.dcm");
    write_jpeg_grid(&path);
    let slide = Slide::open(&path).unwrap();

    let level = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap();
    assert_eq!((level.tile_width, level.tile_height), (16, 16));
    assert_eq!((level.tiles_across, level.tiles_down), (2, 2));
    assert_eq!(level.tiles.len(), 4);
    for tile in &level.tiles {
        assert_eq!(
            tile.location,
            slide
                .tile_location(&tile_request(tile.col, tile.row))
                .unwrap()
        );
    }
}

#[test]
fn multi_fragment_frames_list_each_fragment_value_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fragmented-j2k.dcm");
    assert_eq!(RGB_CODESTREAM.len() % 2, 0, "fixture needs no padding");
    let split = |codestream: &[u8]| {
        let third = (codestream.len() / 3) & !1;
        vec![
            codestream[..third].to_vec(),
            codestream[third..2 * third].to_vec(),
            codestream[2 * third..].to_vec(),
        ]
    };
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: uids::JPEG2000_LOSSLESS,
            rows: 12,
            columns: 16,
            total_pixel_matrix_rows: 12,
            total_pixel_matrix_columns: 32,
            number_of_frames: 2,
            pixel_data: TestPixelData::FragmentedFrames(vec![
                split(RGB_CODESTREAM),
                split(RGB_CODESTREAM),
            ]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let slide = Slide::open(&path).unwrap();
    let file = std::fs::read(&path).unwrap();

    for col in 0..2 {
        let req = tile_request(col, 0);
        let unit = single_unit(slide.tile_location(&req).unwrap());
        assert_eq!(unit.segments().len(), 3);
        assert_eq!(unit.codec().compression(), Compression::Jp2kRgb);
        for (range, fragment) in unit.segments().iter().zip(split(RGB_CODESTREAM)) {
            let header = &file[range.offset as usize - 8..range.offset as usize];
            assert_eq!(
                header[..4],
                [0xFE, 0xFF, 0x00, 0xE0],
                "Item tag precedes it"
            );
            assert_eq!(
                u32::from_le_bytes(header[4..].try_into().unwrap()),
                fragment.len() as u32
            );
            assert_eq!(range.length, fragment.len() as u64);
        }
        let bytes = assembled(&unit);
        assert_eq!(bytes, RGB_CODESTREAM);
        assert_eq!(
            slide.read_raw_compressed_tile(&req).unwrap().data(),
            bytes.as_slice()
        );

        let decoded = crate::core::batch::exactly_one(
            decode_batch_jp2k(&[Jp2kDecodeJob {
                data: std::borrow::Cow::Borrowed(&bytes),
                expected_width: 16,
                expected_height: 12,
                rgb_color_space: true,
                backend: j2k_core::BackendRequest::Cpu,
            }]),
            "located J2K decode",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            decoded.data.as_u8(),
            slide.read_tile(&req).unwrap().data.as_u8()
        );
    }
}

#[test]
fn odd_length_frames_leave_out_the_dicom_padding_byte_like_raw_reads() {
    let dir = tempfile::tempdir().unwrap();
    let codestream = vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9];
    let single = dir.path().join("odd-htj2k.dcm");
    write_test_dicom(
        &single,
        TestDicomOptions::encapsulated(HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX, codestream.clone()),
    );
    // The final fragment holds only the last marker byte and the padding.
    let fragmented = dir.path().join("odd-fragmented-htj2k.dcm");
    write_test_dicom(
        &fragmented,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            pixel_data: TestPixelData::FragmentedFrames(vec![vec![
                codestream[..4].to_vec(),
                vec![0xD9, 0x00],
            ]]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    for (path, lengths) in [(&single, vec![5]), (&fragmented, vec![4, 1])] {
        let slide = Slide::open(path).unwrap();
        let req = tile_request(0, 0);
        let unit = single_unit(slide.tile_location(&req).unwrap());
        assert_eq!(
            unit.segments()
                .iter()
                .map(|range| range.length)
                .collect::<Vec<_>>(),
            lengths
        );
        assert_eq!(assembled(&unit), codestream);
        assert_eq!(
            slide.read_raw_compressed_tile(&req).unwrap().data(),
            codestream
        );
    }
}

#[test]
fn native_frames_are_bare_payload_ranges_of_the_pixel_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native-grid.dcm");
    let pixels = (0..2 * 2 * 2 * 3)
        .map(|value| value as u8 * 9)
        .collect::<Vec<_>>();
    write_test_dicom(
        &path,
        TestDicomOptions {
            total_pixel_matrix_columns: 4,
            number_of_frames: 2,
            ..TestDicomOptions::native(pixels.clone())
        },
    );
    let slide = Slide::open(&path).unwrap();

    for col in 0..2 {
        let req = tile_request(col, 0);
        let unit = single_unit(slide.tile_location(&req).unwrap());
        assert_eq!(unit.framing(), StoredFraming::BarePayload);
        assert_eq!(unit.codec().compression(), Compression::None);
        assert_eq!(unit.codec().byte_order(), None);
        assert_eq!(
            unit.codec().planar_configuration(),
            PlanarConfiguration::Chunky
        );
        let frame = &pixels[col as usize * 12..][..12];
        assert_eq!(assembled(&unit), frame);
        let raw = slide.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(raw.compression(), Compression::None);
        assert_eq!((raw.width(), raw.height(), raw.bits_allocated()), (2, 2, 8));
        assert_eq!(raw.data(), frame);
        assert_eq!(slide.read_tile(&req).unwrap().data.as_u8(), Some(frame));
    }
}

#[test]
fn native_sixteen_bit_and_planar_frames_state_their_stored_layout() {
    let dir = tempfile::tempdir().unwrap();
    let words = [
        0x0102_u16, 0x0304, 0x0506, 0x1112, 0x1314, 0x1516, 0x2122, 0x2324, 0x2526, 0x3132, 0x3334,
        0x3536,
    ];
    let host_bytes = words
        .iter()
        .flat_map(|word| word.to_ne_bytes())
        .collect::<Vec<_>>();

    let little = dir.path().join("native16-le.dcm");
    write_test_dicom(
        &little,
        TestDicomOptions::uint16(TestPixelData::Native(le_words(&words))),
    );
    let big = dir.path().join("native16-be.dcm");
    write_test_dicom(
        &big,
        TestDicomOptions {
            transfer_syntax: EXPLICIT_VR_BIG_ENDIAN_TRANSFER_SYNTAX,
            ..TestDicomOptions::uint16(TestPixelData::Native(
                words.iter().flat_map(|word| word.to_be_bytes()).collect(),
            ))
        },
    );
    for (path, order) in [
        (&little, ByteOrder::LittleEndian),
        (&big, ByteOrder::BigEndian),
    ] {
        let slide = Slide::open(path).unwrap();
        let unit = single_unit(slide.tile_location(&tile_request(0, 0)).unwrap());
        assert_eq!(unit.codec().bits_per_sample(), 16);
        assert_eq!(unit.codec().byte_order(), Some(order));
        let stored = match order {
            ByteOrder::LittleEndian => le_words(&words),
            _ => words.iter().flat_map(|word| word.to_be_bytes()).collect(),
        };
        assert_eq!(assembled(&unit), stored);
        let raw = slide.read_raw_compressed_tile(&tile_request(0, 0)).unwrap();
        assert_eq!(raw.bits_allocated(), 16);
        assert_eq!(raw.data(), host_bytes, "raw samples are in host byte order");
    }

    let planar = dir.path().join("planar.dcm");
    let mut options = TestDicomOptions::native(vec![
        255, 0, 0, 255, // R plane
        0, 255, 0, 255, // G plane
        0, 0, 255, 0, // B plane
    ]);
    options.planar_configuration = Some(1);
    write_test_dicom(&planar, options);
    let slide = Slide::open(&planar).unwrap();
    let unit = single_unit(slide.tile_location(&tile_request(0, 0)).unwrap());
    assert_eq!(
        unit.codec().planar_configuration(),
        PlanarConfiguration::Planar
    );
    assert_eq!(
        assembled(&unit),
        [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 0]
    );
    assert_eq!(
        slide
            .read_raw_compressed_tile(&tile_request(0, 0))
            .unwrap()
            .data(),
        [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
        "raw samples are interleaved"
    );
}

#[test]
fn native_twelve_bit_raw_samples_clear_the_bits_the_codec_marks_insignificant() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native12.dcm");
    let stored = [
        0xf102_u16, 0xe304, 0xd506, 0xc112, 0xb314, 0xa516, 0x9122, 0x8324, 0x7526, 0x6132, 0x5334,
        0x4536,
    ];
    let masked = stored.map(|sample| sample & 0x0fff);
    write_test_dicom(
        &path,
        TestDicomOptions {
            bits_stored: 12,
            high_bit: 11,
            ..TestDicomOptions::uint16(TestPixelData::Native(le_words(&stored)))
        },
    );
    let slide = Slide::open(&path).unwrap();
    let req = tile_request(0, 0);

    let unit = single_unit(slide.tile_location(&req).unwrap());
    assert_eq!(unit.codec().bits_per_sample(), 16);
    assert_eq!(unit.codec().significant_bits(), Some(12));
    assert_eq!(
        assembled(&unit),
        le_words(&stored),
        "locations name stored words"
    );
    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.significant_bits(), Some(12));
    assert_eq!(
        raw.data(),
        masked
            .iter()
            .flat_map(|sample| sample.to_ne_bytes())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        slide.read_tile(&req).unwrap().data.as_u16(),
        Some(masked.as_slice())
    );

    let eight_bit = dir.path().join("native8.dcm");
    write_test_dicom(&eight_bit, TestDicomOptions::native(test_rgb_pixel_data()));
    let eight_bit = Slide::open(&eight_bit).unwrap();
    let unit = single_unit(eight_bit.tile_location(&req).unwrap());
    assert_eq!(unit.codec().significant_bits(), None);
    assert_eq!(
        eight_bit
            .read_raw_compressed_tile(&req)
            .unwrap()
            .significant_bits(),
        None
    );
}

#[test]
fn twelve_bit_rle_raw_frames_keep_stored_words_and_report_significant_bits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rle12.dcm");
    let plane = |base: u16| [base | 0xf000, base | 0xa000, base, base | 0x5000];
    let (r, g, b) = (plane(0x0102), plane(0x0304), plane(0x0506));
    let frame = rle_rgb16_frame(&r, &g, &b);
    assert_eq!(frame.len() % 2, 0, "fixture needs no padding");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: RLE_TRANSFER_SYNTAX,
            bits_stored: 12,
            high_bit: 11,
            ..TestDicomOptions::uint16(TestPixelData::Encapsulated(frame.clone()))
        },
    );
    let slide = Slide::open(&path).unwrap();
    let req = tile_request(0, 0);

    let unit = single_unit(slide.tile_location(&req).unwrap());
    assert_eq!(unit.codec().significant_bits(), Some(12));
    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::Rle);
    assert_eq!(raw.bits_allocated(), 16);
    assert_eq!(raw.significant_bits(), Some(12));
    assert_eq!(
        raw.data(),
        frame.as_slice(),
        "RLE bytes pass through unchanged"
    );

    // Decoding all 16 bits keeps the stored high bits; masking them to the
    // significant bits gives read_tile's samples.
    let decoded = decode_rle_lossless_frame(
        raw.data(),
        2,
        2,
        3,
        "RGB",
        DicomBitDepth::Uint16 { bits_stored: 16 },
    )
    .unwrap();
    let mask = u16::MAX >> (16 - raw.significant_bits().unwrap());
    let masked = decoded
        .data
        .as_u16()
        .unwrap()
        .iter()
        .map(|sample| sample & mask)
        .collect::<Vec<_>>();
    assert_ne!(decoded.data.as_u16().unwrap(), masked.as_slice());
    assert_eq!(
        slide.read_tile(&req).unwrap().data.as_u16(),
        Some(masked.as_slice())
    );
}

#[test]
fn rle_frames_pass_through_as_bare_payloads_with_their_padding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rle.dcm");
    let frame = rle_rgb_frame(&[255, 0, 0, 255], &[0, 255, 0, 255], &[0, 0, 255, 0]);
    let mut stored = frame.clone();
    stored.resize(frame.len().next_multiple_of(2), 0);
    write_test_dicom(
        &path,
        TestDicomOptions::encapsulated(RLE_TRANSFER_SYNTAX, frame),
    );
    let slide = Slide::open(&path).unwrap();
    let req = tile_request(0, 0);

    let unit = single_unit(slide.tile_location(&req).unwrap());
    assert_eq!(unit.framing(), StoredFraming::BarePayload);
    assert_eq!(unit.codec().compression(), Compression::Rle);
    assert_eq!(assembled(&unit), stored);
    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::Rle);
    assert_eq!(raw.data(), stored.as_slice());
    let decoded =
        decode_rle_lossless_frame(raw.data(), 2, 2, 3, "RGB", DicomBitDepth::Eight).unwrap();
    assert_eq!(
        decoded.data.as_u8(),
        slide.read_tile(&req).unwrap().data.as_u8()
    );
}

/// A sparse 8x8 grid storing frame 0 at (0, 0) in two fragments and frame 1
/// at (2, 1) in a fragment that `bad_frame_len` may extend past the end of
/// the 10000-byte file.
fn sparse_reader(bad_frame_len: u32) -> (tempfile::TempDir, DicomReader) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sparse.dcm");
    std::fs::write(&path, vec![0u8; 10_000]).unwrap();
    let tiles = HashMap::from([((0, 0), 0), ((2, 1), 1)]);
    let image = test_dicom_image_with_transfer_syntax(
        "1.2.3.9",
        DicomGrid::Sparse(tiles),
        uids::JPEG2000_LOSSLESS,
    );
    let mut image = Arc::try_unwrap(image).expect("fresh test image");
    image.frame_store.source_path = Arc::from(path.as_path());
    image.frame_store.path = path;
    image.frame_store.source_len = 10_000;
    let fragment = |payload_offset, len| DicomFragmentRef {
        payload_offset,
        item_offset: payload_offset - 8,
        len,
    };
    *image.frame_store.encapsulated_frames.get_mut().unwrap() =
        Some(Arc::new(DicomEncapsulatedFrames {
            fragments: vec![
                fragment(108, 400),
                fragment(516, 300),
                fragment(824, bad_frame_len),
            ],
            frame_ranges: vec![0..2, 2..3],
        }));
    let levels = build_levels(Path::new("sparse.dcm"), vec![Arc::new(image)]).unwrap();
    let reader = DicomReader {
        slide: Arc::new(DicomSlide {
            encoded_unit_bytes: crate::SlideLimits::default().encoded_unit_bytes(),
            dataset: dataset_for_levels(&levels),
            levels,
            associated: HashMap::new(),
        }),
    };
    (dir, reader)
}

#[test]
fn sparse_grid_gaps_are_absent_and_stored_frames_keep_their_fragments() {
    let (dir, reader) = sparse_reader(100);
    let unit = single_unit(reader.tile_location(&tile_request(0, 0)).unwrap());
    assert_eq!(
        unit.segments(),
        [ByteRange::new(108, 400), ByteRange::new(516, 300)]
    );
    assert_eq!(unit.path(), dir.path().join("sparse.dcm"));
    assert_eq!(
        reader.tile_location(&tile_request(1, 0)).unwrap(),
        TileLocation::Absent
    );
    let error = reader
        .read_raw_compressed_tile(&tile_request(1, 0))
        .unwrap_err();
    assert!(error.to_string().contains("sparse missing"), "{error}");

    let slide = Slide::from_source_with_cache_bytes(Box::new(reader), 0);
    let level = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap();
    assert_eq!((level.tiles_across, level.tiles_down), (8, 8));
    let stored = level
        .tiles
        .iter()
        .filter(|tile| matches!(tile.location, TileLocation::Stored(_)))
        .map(|tile| (tile.col, tile.row))
        .collect::<Vec<_>>();
    assert_eq!(stored, [(0, 0), (2, 1)]);
    assert_eq!(
        level
            .tiles
            .iter()
            .filter(|tile| tile.location == TileLocation::Absent)
            .count(),
        62
    );
}

#[test]
fn frames_past_the_end_of_the_file_are_errors() {
    let (_dir, reader) = sparse_reader(9_500);
    let error = reader.tile_location(&tile_request(2, 1)).unwrap_err();
    assert!(
        error.to_string().contains("exceeds the 10000-byte file"),
        "{error}"
    );
    assert!(reader.tile_location(&tile_request(0, 0)).is_ok());

    let slide = Slide::from_source_with_cache_bytes(Box::new(reader), 0);
    let error = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap_err();
    assert!(
        matches!(error, WsiError::TileRead { col: 2, row: 1, .. }),
        "{error}"
    );
}
