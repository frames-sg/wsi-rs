use super::fixtures::*;
use super::*;
use crate::core::types::{ByteRange, EncodedUnit, StoredFraming, TileLocation};

fn assembled(unit: &EncodedUnit) -> Vec<u8> {
    let file = fs::read(unit.path()).expect("read located ETS file");
    unit.assemble(
        unit.segments()
            .iter()
            .map(|range| &file[range.offset as usize..][..range.length as usize]),
    )
    .expect("assemble located codestream")
}

#[test]
fn stored_ets_tiles_locate_their_codestream_and_background_tiles_are_absent() {
    let fixture = write_vsi_fixture(&[("scene", EtsSpec::default())]);
    let ets_path = fixture.path.with_file_name("_synthetic_/scene/frame_t.ets");
    let slide = Slide::open(&fixture.path).unwrap();
    let stored = TileRequest::new(0, 0, 0, 1, 0);
    let background = TileRequest::new(0, 0, 0, 0, 0);

    let TileLocation::Stored(tile) = slide.tile_location(&stored).unwrap() else {
        panic!("tile (1, 0) has a codestream");
    };
    assert_eq!(tile.tile_size(), (16, 12));
    let unit = tile.single_unit().expect("one codestream per tile");
    assert_eq!(unit.path(), ets_path.as_path());
    assert_eq!(unit.framing(), StoredFraming::Standalone);
    assert_eq!(unit.stored_size(), (16, 12));
    let payload_offset = (CHUNK_TABLE_OFFSET + 32) as u64;
    assert_eq!(
        unit.segments(),
        [ByteRange::new(payload_offset, RGB_CODESTREAM.len() as u64)]
    );
    let codec = unit.codec();
    assert_eq!(codec.compression(), Compression::Jp2kRgb);
    assert_eq!((codec.bits_per_sample(), codec.samples_per_pixel()), (8, 3));
    assert_eq!(
        codec.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Rgb
    );

    let bytes = assembled(unit);
    assert_eq!(bytes, RGB_CODESTREAM);
    assert_eq!(
        slide.read_raw_compressed_tile(&stored).unwrap().data(),
        bytes
    );
    let decoded = crate::decode::jp2k::decode_jp2k_to_sample_buffer(
        &bytes,
        16,
        12,
        crate::decode::jp2k::Jp2kColorSpace::Rgb,
    )
    .unwrap();
    assert_eq!(decoded.as_u8(), slide.read_tile(&stored).unwrap().as_u8());

    assert_eq!(
        slide.tile_location(&background).unwrap(),
        TileLocation::Absent
    );
    let error = slide.read_raw_compressed_tile(&background).unwrap_err();
    assert!(
        matches!(&error, WsiError::Unsupported { reason } if reason.contains("background") && reason.contains("Absent")),
        "{error}"
    );
}

#[test]
fn level_tile_locations_cover_every_ets_level() {
    let pyramid = EtsSpec {
        n_dimensions: 4,
        use_pyramid: true,
        chunks: vec![
            ChunkSpec::new(&[1, 0, 0, 0], RGB_CODESTREAM),
            ChunkSpec::new(&[0, 0, 0, 1], RGB_CODESTREAM),
        ],
        ..EtsSpec::default()
    };
    let fixture = write_vsi_fixture(&[("pyramid", pyramid)]);
    let slide = Slide::open(&fixture.path).unwrap();

    let level0 = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap();
    assert_eq!((level0.tiles_across, level0.tiles_down), (2, 1));
    assert_eq!(level0.tiles[0].location, TileLocation::Absent);
    let TileLocation::Stored(first) = &level0.tiles[1].location else {
        panic!("level 0 tile (1, 0) is stored");
    };
    let level1 = slide
        .level_tile_locations(0, 0, 1, PlaneSelection::default())
        .unwrap();
    assert_eq!((level1.tiles_across, level1.tiles_down), (1, 1));
    let TileLocation::Stored(second) = &level1.tiles[0].location else {
        panic!("level 1 tile (0, 0) is stored");
    };
    let (first, second) = (first.single_unit().unwrap(), second.single_unit().unwrap());
    assert_ne!(first.segments(), second.segments());
    assert_eq!(assembled(first), assembled(second));
}

#[test]
fn ets_codec_describes_declared_samples() {
    for (pixel_type, samples_per_pixel, expected) in [
        (
            3,
            3,
            Some((16, 3, EncodedTilePhotometricInterpretation::Rgb)),
        ),
        (
            1,
            1,
            Some((8, 1, EncodedTilePhotometricInterpretation::Monochrome2)),
        ),
        (9, 3, None),
    ] {
        let spec = EtsSpec {
            pixel_type,
            samples_per_pixel,
            ..EtsSpec::default()
        };
        let fixture = write_vsi_fixture(&[("scene", spec)]);
        let reader = OlympusVsiBackend.open(&fixture.path).unwrap();
        let location = reader.tile_location(&TileRequest::new(0, 0, 0, 1, 0));
        match expected {
            Some((bits, samples, photometric)) => {
                let TileLocation::Stored(tile) = location.unwrap() else {
                    panic!("tile (1, 0) is stored");
                };
                let codec = tile.single_unit().unwrap().codec();
                assert_eq!(codec.bits_per_sample(), bits);
                assert_eq!(codec.samples_per_pixel(), samples);
                assert_eq!(codec.photometric_interpretation(), photometric);
            }
            None => assert!(matches!(location, Err(WsiError::Unsupported { .. }))),
        }
    }
}

#[test]
fn ets_ranges_past_the_end_of_the_file_are_errors() {
    let (_temp, path) = write_ets(&build_ets(&EtsSpec::default()));
    let scene = EtsScene::parse(&path).unwrap();
    let past_end = scene::EtsTile {
        offset: scene.file_len - 4,
        byte_count: 16,
    };
    let error = scene.tile_location(&past_end, 16, 12).unwrap_err();
    assert!(error.to_string().contains("exceeds"), "{error}");
}
