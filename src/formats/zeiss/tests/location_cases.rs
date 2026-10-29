use super::fixtures::{jpeg_rgb, main_fixture, metadata_xml, write_fixture, SubblockSpec};
use crate::core::registry::Slide;
use crate::core::types::*;
use crate::formats::zeiss::slide::{ZeissReader, ZeissSlide};
use crate::{SlideReader, WsiError};
use std::sync::Arc;

const JXR: &[u8] = include_bytes!("../../../../tests/fixtures/jxr/rgb.jxr");

fn stored_units(location: TileLocation) -> (StoredTile, Vec<EncodedUnit>) {
    let TileLocation::Stored(stored) = location else {
        panic!("expected a stored CZI tile, got {location:?}");
    };
    let units = stored.units().to_vec();
    (stored, units)
}

fn segment_bytes<'a>(file: &'a [u8], unit: &EncodedUnit) -> Vec<&'a [u8]> {
    unit.segments()
        .iter()
        .map(|range| &file[range.offset as usize..range.end().unwrap() as usize])
        .collect()
}

fn assembled(file: &[u8], unit: &EncodedUnit) -> Vec<u8> {
    unit.assemble(segment_bytes(file, unit)).unwrap()
}

fn unsupported_reason(error: WsiError) -> String {
    match error {
        WsiError::Unsupported { reason } => reason,
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn jpeg_subblock_location_is_its_data_part_and_round_trips() {
    let jpeg = jpeg_rgb(8, 8, &[200, 80, 20].repeat(8 * 8));
    let mut block = SubblockSpec::bgr24(0, 0, 8, 8, jpeg.clone());
    block.compression = 1;
    block.metadata = b"<METADATA><Tags/></METADATA>".to_vec();
    let fixture = write_fixture(&[block], &[], &metadata_xml(8, 8));
    let slide = Slide::open(fixture.path()).unwrap();
    let req = TileRequest::new(0, 0, 0, 0, 0);

    let (stored, units) = stored_units(slide.tile_location(&req).unwrap());
    assert_eq!(stored.tile_size(), (8, 8));
    let [unit] = units.as_slice() else {
        panic!("one subblock paints the tile");
    };
    assert_eq!(unit.path(), fixture.path());
    assert_eq!(unit.framing(), StoredFraming::Standalone);
    assert_eq!(unit.codec().compression(), Compression::Jpeg);
    assert_eq!(
        (
            unit.codec().bits_per_sample(),
            unit.codec().samples_per_pixel()
        ),
        (8, 3)
    );
    assert_eq!(unit.stored_size(), (8, 8));
    assert_eq!(unit.source_region(), PixelRect::new(0, 0, 8, 8));
    assert_eq!(unit.tile_region(), PixelRect::new(0, 0, 8, 8));

    let file = std::fs::read(fixture.path()).unwrap();
    assert_eq!(assembled(&file, unit), jpeg, "header and metadata excluded");
    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.data(), jpeg);
    let decoded = crate::decode::jpeg::decode_standalone_jpeg(raw.data()).unwrap();
    assert_eq!(decoded.as_u8(), slide.read_tile(&req).unwrap().as_u8());
}

#[test]
fn whole_jpegxr_subblock_passes_through_unchanged() {
    let mut block = SubblockSpec::bgr24(0, 0, 16, 16, JXR.to_vec());
    block.compression = 4;
    let fixture = write_fixture(&[block], &[], &metadata_xml(16, 16));
    let slide = Slide::open(fixture.path()).unwrap();
    let req = TileRequest::new(0, 0, 0, 0, 0);

    let (_, units) = stored_units(slide.tile_location(&req).unwrap());
    let [unit] = units.as_slice() else {
        panic!("one subblock paints the tile");
    };
    assert_eq!(unit.codec().compression(), Compression::JpegXr);
    assert_eq!(unit.framing(), StoredFraming::Standalone);

    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::JpegXr);
    assert_eq!((raw.width(), raw.height()), (16, 16));
    assert_eq!(raw.tile_region(), PixelRect::new(0, 0, 16, 16));
    let file = std::fs::read(fixture.path()).unwrap();
    assert_eq!(assembled(&file, unit), raw.data());
    assert_eq!(raw.data(), JXR);
}

/// A 1x1 anchor at the canvas origin and a 16x16 subblock at x = 250, so
/// tile (1, 0) is the 10x16 right part of the subblock and tile (0, 0)
/// combines the anchor, background and the subblock's left part.
fn straddling_fixture(compression: i32, data: Vec<u8>) -> tempfile::NamedTempFile {
    let mut block = SubblockSpec::bgr24(250, 0, 16, 16, data);
    block.compression = compression;
    let anchor = SubblockSpec::bgr24(0, 0, 1, 1, vec![3, 2, 1]);
    write_fixture(&[anchor, block], &[], &metadata_xml(266, 16))
}

#[test]
fn tile_inside_part_of_a_jpegxr_subblock_returns_the_whole_stream() {
    let fixture = straddling_fixture(4, JXR.to_vec());
    let slide = Slide::open(fixture.path()).unwrap();
    let req = TileRequest::new(0, 0, 0, 1, 0);

    let (stored, units) = stored_units(slide.tile_location(&req).unwrap());
    assert_eq!(stored.tile_size(), (10, 16));
    let [unit] = units.as_slice() else {
        panic!("one subblock paints the tile");
    };
    assert_eq!(unit.stored_size(), (16, 16));
    assert_eq!(unit.source_region(), PixelRect::new(6, 0, 10, 16));
    assert_eq!(unit.tile_region(), PixelRect::new(0, 0, 10, 16));

    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::JpegXr);
    assert_eq!((raw.width(), raw.height()), (16, 16));
    assert_eq!(raw.tile_region(), PixelRect::new(6, 0, 10, 16));
    assert_eq!(raw.data(), JXR);

    let decoded = crate::decode::jpegxr::decode_jpegxr(
        raw.data(),
        16,
        16,
        SampleType::Uint8,
        3,
        Default::default(),
    )
    .unwrap();
    let expected: Vec<u8> = (0..16)
        .flat_map(|y| decoded.as_u8().unwrap()[(y * 16 + 6) * 3..(y * 16 + 16) * 3].to_vec())
        .collect();
    assert_eq!(
        slide.read_tile(&req).unwrap().as_u8(),
        Some(expected.as_slice())
    );
}

#[test]
fn lossy_compositions_point_to_tile_location() {
    let jpeg = jpeg_rgb(16, 16, &[200, 80, 20].repeat(16 * 16));
    for (compression, data) in [(4, JXR.to_vec()), (1, jpeg)] {
        let fixture = straddling_fixture(compression, data);
        let slide = Slide::open(fixture.path()).unwrap();

        let composed = TileRequest::new(0, 0, 0, 0, 0);
        let (_, units) = stored_units(slide.tile_location(&composed).unwrap());
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].codec().compression(), Compression::None);
        assert_eq!(units[0].tile_region(), PixelRect::new(0, 0, 1, 1));
        assert_eq!(units[1].source_region(), PixelRect::new(0, 0, 6, 16));
        assert_eq!(units[1].tile_region(), PixelRect::new(250, 0, 6, 16));
        let reason = unsupported_reason(slide.read_raw_compressed_tile(&composed).unwrap_err());
        assert!(reason.contains("tile_location"), "{reason}");
    }
}

#[test]
fn tile_inside_part_of_a_jpeg_subblock_is_cropped_losslessly() {
    use j2k_jpeg::transcode::{extract_dct_blocks, DctExtractOptions};
    // A 48x16 JPEG at x = 220 puts canvas columns 256..268 (tile (1, 0)) at
    // subblock columns 36..48.
    let pixels: Vec<u8> = (0..48 * 16)
        .flat_map(|index| [(index * 7) as u8, (index * 3) as u8, (255 - index) as u8])
        .collect();
    let jpeg = jpeg_rgb(48, 16, &pixels);
    let mut block = SubblockSpec::bgr24(220, 0, 48, 16, jpeg.clone());
    block.compression = 1;
    let anchor = SubblockSpec::bgr24(0, 0, 1, 1, vec![3, 2, 1]);
    let fixture = write_fixture(&[anchor, block], &[], &metadata_xml(268, 16));
    let slide = Slide::open(fixture.path()).unwrap();
    let req = TileRequest::new(0, 0, 0, 1, 0);

    let (_, units) = stored_units(slide.tile_location(&req).unwrap());
    let [unit] = units.as_slice() else {
        panic!("one subblock paints the tile");
    };
    assert_eq!(unit.source_region(), PixelRect::new(36, 0, 12, 16));

    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::Jpeg);
    let stored = extract_dct_blocks(&jpeg, DctExtractOptions::default()).unwrap();
    let cropped = extract_dct_blocks(raw.data(), DctExtractOptions::default()).unwrap();
    // The crop starts at the MCU corner at or before x = 36.
    let max_h = stored.components.iter().map(|c| c.h_samp).max().unwrap();
    let mcu_width = 8 * u32::from(max_h);
    let crop_x = 36 / mcu_width * mcu_width;
    assert_eq!(raw.tile_region(), PixelRect::new(36 - crop_x, 0, 12, 16));
    assert_eq!((raw.width(), raw.height()), (48 - crop_x, 16));
    // Quantized coefficients are copied from the stored JPEG unchanged.
    let mcu_col = (crop_x / mcu_width) as usize;
    for (src, out) in stored.components.iter().zip(&cropped.components) {
        assert_eq!((out.h_samp, out.v_samp), (src.h_samp, src.v_samp));
        assert_eq!(out.quant_table, src.quant_table);
        let offset = mcu_col * usize::from(src.h_samp);
        for by in 0..out.block_rows as usize {
            for bx in 0..out.block_cols as usize {
                assert_eq!(
                    out.quantized_blocks[by * out.block_cols as usize + bx],
                    src.quantized_blocks[by * src.block_cols as usize + bx + offset]
                );
            }
        }
    }
}

#[test]
fn uncompressed_tiles_locate_bgr_payloads_and_return_exact_rgb_samples() {
    let fixture = main_fixture();
    let slide = Slide::open(fixture.path()).unwrap();
    let file = std::fs::read(fixture.path()).unwrap();
    let req = TileRequest::new(0, 0, 0, 0, 0);

    let (stored, units) = stored_units(slide.tile_location(&req).unwrap());
    assert_eq!(stored.tile_size(), (4, 2));
    assert_eq!(units.len(), 2);
    for (unit, x, bgr) in [
        (&units[0], 0, vec![3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10]),
        (
            &units[1],
            2,
            vec![15, 14, 13, 18, 17, 16, 21, 20, 19, 24, 23, 22],
        ),
    ] {
        assert_eq!(unit.framing(), StoredFraming::BarePayload);
        assert_eq!(unit.codec().compression(), Compression::None);
        assert_eq!(unit.codec().channel_order(), ChannelOrder::Bgr);
        assert_eq!(unit.stored_size(), (2, 2));
        assert_eq!(unit.tile_region(), PixelRect::new(x, 0, 2, 2));
        assert_eq!(assembled(&file, unit), bgr);
    }

    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::None);
    assert_eq!((raw.width(), raw.height(), raw.bits_allocated()), (4, 2, 8));
    assert_eq!(Some(raw.data()), slide.read_tile(&req).unwrap().as_u8());
    assert_eq!(&raw.data()[..6], &[1, 2, 3, 4, 5, 6]);

    // The reduced subblock is the whole level-1 tile.
    let reduced = TileRequest::new(0, 0, 1, 0, 0);
    let (_, units) = stored_units(slide.tile_location(&reduced).unwrap());
    let [unit] = units.as_slice() else {
        panic!("one reduced subblock paints the level-1 tile");
    };
    assert_eq!(assembled(&file, unit), vec![30, 29, 28, 33, 32, 31]);
    let raw = slide.read_raw_compressed_tile(&reduced).unwrap();
    assert_eq!(raw.data(), [28, 29, 30, 31, 32, 33]);
}

#[test]
fn hidden_subblocks_are_left_out_in_paint_order() {
    let mut high = SubblockSpec::bgr24(0, 0, 1, 1, vec![30, 20, 10]);
    high.m_index = Some(2);
    let mut low = SubblockSpec::bgr24(0, 0, 1, 1, vec![3, 2, 1]);
    low.m_index = Some(1);
    let fixture = write_fixture(&[high, low], &[], &metadata_xml(1, 1));
    let slide = Slide::open(fixture.path()).unwrap();
    let (_, units) = stored_units(
        slide
            .tile_location(&TileRequest::new(0, 0, 0, 0, 0))
            .unwrap(),
    );
    let [unit] = units.as_slice() else {
        panic!("the lower subblock is hidden");
    };
    let file = std::fs::read(fixture.path()).unwrap();
    assert_eq!(assembled(&file, unit), vec![30, 20, 10]);
}

#[test]
fn level_locations_mark_tiles_without_subblocks_absent() {
    let left = SubblockSpec::bgr24(0, 0, 1, 1, vec![3, 2, 1]);
    let right = SubblockSpec::bgr24(600, 0, 1, 1, vec![6, 5, 4]);
    let fixture = write_fixture(&[left, right], &[], &metadata_xml(601, 1));
    let slide = Slide::open(fixture.path()).unwrap();

    let level = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap();
    assert_eq!((level.tiles_across, level.tiles_down), (3, 1));
    let kinds: Vec<_> = level
        .tiles
        .iter()
        .map(|tile| match tile.location {
            TileLocation::Stored(_) => "stored",
            TileLocation::Absent => "absent",
            TileLocation::NotAddressable(_) => "not addressable",
        })
        .collect();
    assert_eq!(kinds, ["stored", "absent", "stored"]);
    let reason = unsupported_reason(
        slide
            .read_raw_compressed_tile(&TileRequest::new(0, 0, 0, 1, 0))
            .unwrap_err(),
    );
    assert!(reason.contains("Absent"), "{reason}");
}

#[test]
fn data_range_past_the_file_end_is_an_error() {
    let fixture = main_fixture();
    let mut slide = ZeissSlide::parse(fixture.path()).unwrap();
    let file_len = std::fs::metadata(fixture.path()).unwrap().len();
    slide.subblock_data[0] = ByteRange::new(file_len - 4, 12);
    let reader = ZeissReader {
        slide: Arc::new(slide),
    };
    let error = reader
        .tile_location(&TileRequest::new(0, 0, 0, 0, 0))
        .expect_err("a range past EOF is not a location");
    assert!(error.to_string().contains("exceeds"), "{error}");
}

/// A 48x16 JPEG subblock at x = 220, so canvas columns 256..268 (tile
/// (1, 0)) are its columns 36..48, beside an uncompressed 1x1 anchor.
fn jpeg_crop_fixture() -> (tempfile::NamedTempFile, Vec<u8>) {
    let pixels: Vec<u8> = (0..48 * 16)
        .flat_map(|index| [(index * 7) as u8, (index * 3) as u8, (255 - index) as u8])
        .collect();
    let jpeg = jpeg_rgb(48, 16, &pixels);
    let mut block = SubblockSpec::bgr24(220, 0, 48, 16, jpeg.clone());
    block.compression = 1;
    let anchor = SubblockSpec::bgr24(0, 0, 1, 1, vec![3, 2, 1]);
    let fixture = write_fixture(&[anchor, block], &[], &metadata_xml(268, 16));
    (fixture, jpeg)
}

fn open_with_limits(fixture: &tempfile::NamedTempFile, limits: crate::SlideLimits) -> Slide {
    Slide::open_with_options(
        fixture.path(),
        crate::SlideOpenOptions::default().with_limits(limits),
    )
    .unwrap()
}

#[test]
fn jpeg_crops_over_the_lossless_crop_limit_return_the_whole_subblock() {
    let (fixture, jpeg) = jpeg_crop_fixture();
    let req = TileRequest::new(0, 0, 0, 1, 0);
    // Leave one byte for coefficients after the subblock, the marker
    // segments parsing copies out of it, and the worst-case output.
    let source_len = jpeg.len() as u64;
    let working_set = 2 * source_len
        + crate::decode::jpeg::lossless::lossless_crop_output_bound(source_len, 12, 16)
        + 1;
    let slide = open_with_limits(
        &fixture,
        crate::SlideLimits::default()
            .with_lossless_crop_bytes(working_set)
            .unwrap(),
    );
    let (_, units) = stored_units(slide.tile_location(&req).unwrap());
    let [unit] = units.as_slice() else {
        panic!("one subblock paints the tile");
    };
    let file = std::fs::read(fixture.path()).unwrap();

    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!(raw.data(), assembled(&file, unit));
    assert_eq!(raw.data(), jpeg);
    assert_eq!((raw.width(), raw.height()), (48, 16));
    assert_eq!(raw.tile_region(), unit.source_region());
    let decoded = crate::decode::jpeg::decode_standalone_jpeg(raw.data()).unwrap();
    let decoded = decoded.as_u8().unwrap();
    let region = raw.tile_region();
    let expected: Vec<u8> = (region.y..region.y + region.height)
        .flat_map(|y| {
            let start = ((y * 48 + region.x) * 3) as usize;
            decoded[start..start + region.width as usize * 3].to_vec()
        })
        .collect();
    assert_eq!(
        slide.read_tile(&req).unwrap().as_u8(),
        Some(expected.as_slice())
    );

    // Under the default limit the same tile is cropped, with the same color.
    let crop = Slide::open(fixture.path())
        .unwrap()
        .read_raw_compressed_tile(&req)
        .unwrap();
    assert!(crop.width() < 48, "{}", crop.width());
    assert_eq!(
        crop.photometric_interpretation(),
        raw.photometric_interpretation()
    );
}

#[test]
fn only_raw_reads_of_jpeg_crops_reserve_the_lossless_crop_working_set() {
    let (fixture, _) = jpeg_crop_fixture();
    let crop = TileRequest::new(0, 0, 0, 1, 0);
    // Painted by the anchor and part of the JPEG subblock: no crop.
    let composed = TileRequest::new(0, 0, 0, 0, 0);
    let limits = crate::SlideLimits::default();
    let slide = Slide::open(fixture.path()).unwrap();
    assert_eq!(
        slide
            .source()
            .raw_compressed_tile_reservation_hint(&crop)
            .unwrap(),
        limits.lossless_crop_bytes()
    );
    assert_eq!(
        slide
            .source()
            .raw_compressed_tile_reservation_hint(&composed)
            .unwrap(),
        0
    );

    // Room for one encoded unit per operation, but not the crop working set.
    let between = (limits.encoded_unit_bytes() + limits.lossless_crop_bytes()) / 2;
    let slide = open_with_limits(
        &fixture,
        limits.with_operation_transient_bytes(between).unwrap(),
    );
    slide
        .read_tile(&crop)
        .expect("decoded reads reserve one encoded unit");
    let error = slide.read_raw_compressed_tile(&crop).unwrap_err();
    assert!(
        matches!(
            error,
            WsiError::ResourceLimit {
                resource: "per-operation transient work",
                requested,
                limit,
            } if requested == limits.lossless_crop_bytes() && limit == between
        ),
        "{error:?}"
    );
}
