use super::super::*;
use std::path::Path;
use std::sync::Arc;

fn path() -> Arc<Path> {
    Arc::from(Path::new("/slides/a.tif"))
}

fn rgb_jpeg_codec() -> EncodedCodec {
    EncodedCodec::new(
        Compression::Jpeg,
        8,
        3,
        EncodedTilePhotometricInterpretation::YbrFull422,
    )
}

fn unit(segments: Vec<ByteRange>) -> EncodedUnitBuilder {
    EncodedUnit::builder(path(), rgb_jpeg_codec(), 16, 16).segments(segments)
}

#[test]
fn pixel_rect_fits_within_rejects_empty_and_overflowing_rects() {
    assert!(PixelRect::new(0, 0, 16, 16).fits_within(16, 16));
    assert!(PixelRect::new(8, 8, 8, 8).fits_within(16, 16));
    assert!(!PixelRect::new(8, 8, 9, 8).fits_within(16, 16));
    assert!(!PixelRect::new(0, 0, 0, 8).fits_within(16, 16));
    assert!(!PixelRect::new(u32::MAX, 0, 2, 1).fits_within(u32::MAX, 1));
}

#[test]
fn encoded_unit_defaults_regions_to_the_stored_image() {
    let unit = unit(vec![ByteRange::new(100, 50)]).build(150).unwrap();

    assert_eq!(unit.path(), Path::new("/slides/a.tif"));
    assert_eq!(unit.segments(), &[ByteRange::new(100, 50)]);
    assert_eq!(unit.framing(), StoredFraming::Standalone);
    assert_eq!(unit.stored_size(), (16, 16));
    assert_eq!(unit.source_region(), PixelRect::new(0, 0, 16, 16));
    assert_eq!(unit.tile_region(), PixelRect::new(0, 0, 16, 16));
    assert_eq!(unit.stored_len(), Some(50));
    assert!(unit.prefix().is_empty() && unit.suffix().is_empty());
}

#[test]
fn encoded_unit_build_checks_segments_against_the_file() {
    let error =
        |builder: EncodedUnitBuilder, file_len| builder.build(file_len).unwrap_err().to_string();

    assert!(error(unit(Vec::new()), 100).contains("no byte segments"));
    assert!(error(unit(vec![ByteRange::new(0, 0)]), 100).contains("segment 0 is empty"));
    assert!(error(unit(vec![ByteRange::new(90, 11)]), 100).contains("exceeds the 100-byte file"));
    assert!(error(unit(vec![ByteRange::new(u64::MAX, 2)]), u64::MAX).contains("exceeds"));
    assert!(error(
        unit(vec![ByteRange::new(0, u64::MAX), ByteRange::new(0, 1)]),
        u64::MAX
    )
    .contains("overflow"));
}

#[test]
fn encoded_unit_build_checks_framing_and_regions() {
    let segments = vec![ByteRange::new(0, 10)];
    let prefix: Arc<[u8]> = Arc::from(&[0xFF, 0xD8][..]);
    let empty: Arc<[u8]> = Arc::from(&[][..]);

    let stray_prefix = unit(segments.clone()).prefix_suffix(prefix.clone(), empty.clone());
    assert!(stray_prefix
        .build(10)
        .unwrap_err()
        .to_string()
        .contains("Prefixed"));

    let missing_tables = unit(segments.clone()).framing(StoredFraming::JpegTables);
    assert!(missing_tables
        .build(10)
        .unwrap_err()
        .to_string()
        .contains("JPEG tables"));

    let outside = unit(segments.clone()).source_region(PixelRect::new(8, 0, 9, 16));
    assert!(outside
        .build(10)
        .unwrap_err()
        .to_string()
        .contains("outside"));

    let empty_tile = unit(segments.clone()).tile_region(PixelRect::new(0, 0, 0, 0));
    assert!(empty_tile
        .build(10)
        .unwrap_err()
        .to_string()
        .contains("tile region"));

    let prefixed = unit(segments)
        .framing(StoredFraming::Prefixed)
        .prefix_suffix(prefix, empty)
        .build(10)
        .unwrap();
    assert_eq!(prefixed.prefix(), &[0xFF, 0xD8]);
}

#[test]
fn assemble_concatenates_and_frames_supplied_segment_bytes() {
    let standalone = unit(vec![ByteRange::new(0, 2), ByteRange::new(10, 3)])
        .build(13)
        .unwrap();
    assert_eq!(
        standalone
            .assemble([&[1u8, 2][..], &[3, 4, 5][..]])
            .unwrap(),
        vec![1, 2, 3, 4, 5]
    );

    let prefixed = unit(vec![ByteRange::new(0, 2)])
        .framing(StoredFraming::Prefixed)
        .prefix_suffix(Arc::from(&[9u8][..]), Arc::from(&[7u8, 7][..]))
        .build(2)
        .unwrap();
    assert_eq!(
        prefixed.assemble([&[1u8, 2][..]]).unwrap(),
        vec![9, 1, 2, 7, 7]
    );
}

#[test]
fn assemble_rejects_segment_bytes_that_do_not_match_the_ranges() {
    let unit = unit(vec![ByteRange::new(0, 2), ByteRange::new(10, 3)])
        .build(13)
        .unwrap();

    let too_few = unit.assemble([&[1u8, 2][..]]).unwrap_err().to_string();
    assert!(
        too_few.contains("2 segments, but 1 were supplied"),
        "{too_few}"
    );
    let wrong_len = unit
        .assemble([&[1u8, 2][..], &[3u8][..]])
        .unwrap_err()
        .to_string();
    assert!(
        wrong_len.contains("segment 1 is 1 bytes, expected 3"),
        "{wrong_len}"
    );
    let too_many = unit
        .assemble([&[1u8, 2][..], &[3u8, 4, 5][..], &[6u8][..]])
        .unwrap_err()
        .to_string();
    assert!(too_many.contains("more were supplied"), "{too_many}");
}

#[test]
fn stored_tile_requires_units_inside_the_tile() {
    let whole = unit(vec![ByteRange::new(0, 4)]).build(4).unwrap();
    let tile = StoredTile::new(16, 16, vec![whole.clone()]).unwrap();
    assert_eq!(tile.tile_size(), (16, 16));
    assert_eq!(tile.single_unit(), Some(&whole));

    let lower = unit(vec![ByteRange::new(0, 4)])
        .tile_region(PixelRect::new(0, 8, 16, 8))
        .build(4)
        .unwrap();
    let pair = StoredTile::new(16, 16, vec![whole.clone(), lower.clone()]).unwrap();
    assert_eq!(pair.units().len(), 2);
    assert_eq!(pair.single_unit(), None);

    assert!(StoredTile::new(16, 12, vec![lower]).is_err());
    assert!(StoredTile::new(16, 16, Vec::new()).is_err());
}

#[test]
fn encoded_codec_records_decode_parameters() {
    let tables: Arc<[u8]> = Arc::from(&[0xFF, 0xD8, 0xFF, 0xD9][..]);
    let codec = EncodedCodec::new(
        Compression::Lzw,
        16,
        3,
        EncodedTilePhotometricInterpretation::Rgb,
    )
    .with_planar_configuration(PlanarConfiguration::Planar)
    .with_channel_order(ChannelOrder::Bgr)
    .with_significant_bits(12)
    .with_predictor(Predictor::Horizontal)
    .with_byte_order(ByteOrder::BigEndian)
    .with_chroma_subsampling(2, 1)
    .with_jpeg_tables(tables);

    assert_eq!(codec.compression(), Compression::Lzw);
    assert_eq!(codec.bits_per_sample(), 16);
    assert_eq!(codec.samples_per_pixel(), 3);
    assert_eq!(codec.planar_configuration(), PlanarConfiguration::Planar);
    assert_eq!(codec.channel_order(), ChannelOrder::Bgr);
    assert_eq!(codec.significant_bits(), Some(12));
    assert_eq!(codec.predictor(), Predictor::Horizontal);
    assert_eq!(codec.byte_order(), Some(ByteOrder::BigEndian));
    assert_eq!(codec.chroma_subsampling(), Some((2, 1)));
    assert_eq!(codec.jpeg_tables(), Some(&[0xFF, 0xD8, 0xFF, 0xD9][..]));
}

#[test]
fn raw_compressed_tile_region_defaults_to_the_encoded_image() {
    let build = |region: Option<PixelRect>| {
        let builder = RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(32, 16)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::YbrFull422)
            .data(vec![1]);
        match region {
            Some(region) => builder.tile_region(region).build(),
            None => builder.build(),
        }
    };

    assert_eq!(
        build(None).unwrap().tile_region(),
        PixelRect::new(0, 0, 32, 16)
    );
    assert_eq!(
        build(Some(PixelRect::new(4, 0, 28, 16)))
            .unwrap()
            .tile_region(),
        PixelRect::new(4, 0, 28, 16)
    );
    assert_eq!(
        build(Some(PixelRect::new(4, 0, 29, 16))),
        Err(RawCompressedTileBuildError::InvalidTileRegion)
    );
}

#[test]
fn raw_compressed_tile_significant_bits_must_fit_the_bit_depth() {
    let build = |bits: u16| {
        RawCompressedTile::builder(Compression::Rle)
            .dimensions(4, 4)
            .bits_allocated(16)
            .significant_bits(bits)
            .samples_per_pixel(1)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::Monochrome2)
            .data(vec![1])
            .build()
    };

    assert_eq!(build(12).unwrap().significant_bits(), Some(12));
    assert_eq!(build(16).unwrap().significant_bits(), Some(16));
    assert_eq!(
        build(0),
        Err(RawCompressedTileBuildError::InvalidSignificantBits)
    );
    assert_eq!(
        build(17),
        Err(RawCompressedTileBuildError::InvalidSignificantBits)
    );
}
