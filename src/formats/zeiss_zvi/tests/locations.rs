use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

use super::super::sectors::CfbSectorMap;
use super::super::*;
use super::fixtures::*;
use crate::core::registry::Slide;

fn assembled(unit: &EncodedUnit) -> Vec<u8> {
    let file = fs::read(unit.path()).expect("read located ZVI file");
    unit.assemble(
        unit.segments()
            .iter()
            .map(|range| &file[range.offset as usize..][..range.length as usize]),
    )
    .expect("assemble located plane payload")
}

fn stored(slide: &Slide, req: &TileRequest) -> StoredTile {
    match slide.tile_location(req).expect("ZVI tile location") {
        TileLocation::Stored(stored) => stored,
        other => panic!("expected a stored ZVI plane, got {other:?}"),
    }
}

fn plane_request(col: i64, c: u32) -> TileRequest {
    TileRequest::new(0, 0, 0, col, 0).with_plane(PlaneSelection::new(0, c, 0))
}

fn u16_le(samples: &[u16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

fn u16_ne(samples: &[u16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_ne_bytes())
        .collect()
}

fn raw_u16_plane(width: u32, height: u32, c: u32, samples: &[u16]) -> PlaneSpec {
    PlaneSpec {
        bytes_per_sample: 2,
        c,
        encoding: PlaneEncoding::Raw(u16_le(samples)),
        ..PlaneSpec::raw_u8(width, height, Vec::new())
    }
}

fn whole_level_tags(width: u32, height: u32) -> [(i32, String); 2] {
    [(515, width.to_string()), (516, height.to_string())]
}

#[test]
fn whole_planes_locate_their_payloads_in_the_mini_stream() {
    for version in [cfb::Version::V3, cfb::Version::V4] {
        assert_whole_planes_round_trip(&ZviFixture::whole_u8_with_version(version));
    }
}

fn assert_whole_planes_round_trip(fixture: &ZviFixture) {
    let slide = Slide::open(&fixture.path).unwrap();
    let cases = [
        (Compression::None, StoredFraming::BarePayload),
        (Compression::Deflate, StoredFraming::BarePayload),
        (Compression::Jpeg, StoredFraming::Standalone),
    ];
    for (c, (compression, framing)) in (0u32..).zip(cases) {
        let req = plane_request(0, c);
        let tile = stored(&slide, &req);
        assert_eq!(tile.tile_size(), (8, 4));
        let unit = tile.single_unit().expect("one plane per tile");
        assert_eq!(unit.path(), fixture.path.as_path());
        assert_eq!(unit.framing(), framing);
        assert_eq!(unit.stored_size(), (8, 4));
        let codec = unit.codec();
        assert_eq!(codec.compression(), compression);
        assert_eq!((codec.bits_per_sample(), codec.samples_per_pixel()), (8, 1));
        assert_eq!(
            codec.photometric_interpretation(),
            EncodedTilePhotometricInterpretation::Monochrome2
        );
        assert_eq!(codec.byte_order(), None);

        let bytes = assembled(unit);
        let raw = slide.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(raw.compression(), compression);
        assert_eq!((raw.width(), raw.height()), (8, 4));
        assert_eq!(raw.data(), bytes.as_slice());
        let decoded = match compression {
            Compression::None => bytes,
            Compression::Deflate => {
                let mut inflated = Vec::new();
                flate2::read::ZlibDecoder::new(bytes.as_slice())
                    .read_to_end(&mut inflated)
                    .unwrap();
                inflated
            }
            _ => crate::core::batch::exactly_one(
                crate::decode::jpeg::decode_batch_jpeg(&[JpegDecodeJob::with_expected(
                    &bytes, 8, 4,
                )]),
                "located ZVI JPEG",
            )
            .unwrap()
            .unwrap()
            .as_u8()
            .unwrap()
            .to_vec(),
        };
        assert_eq!(
            Some(decoded.as_slice()),
            slide.read_tile(&req).unwrap().as_u8()
        );
    }
}

#[test]
fn virtual_tiles_use_a_window_of_a_regular_sector_payload() {
    for version in [cfb::Version::V3, cfb::Version::V4] {
        assert_window_of_regular_sector_payload(version);
    }
}

fn assert_window_of_regular_sector_payload(version: cfb::Version) {
    let samples = (0..300u16 * 20)
        .map(|value| value.wrapping_mul(7))
        .collect::<Vec<_>>();
    let fixture = ZviFixture::write_with_version(
        &[raw_u16_plane(300, 20, 0, &samples)],
        &whole_level_tags(300, 20),
        None,
        version,
    );
    let slide = Slide::open(&fixture.path).unwrap();
    let req = plane_request(1, 0);

    let tile = stored(&slide, &req);
    assert_eq!(tile.tile_size(), (44, 20));
    let unit = tile.single_unit().unwrap();
    assert_eq!(unit.stored_size(), (300, 20));
    assert_eq!(unit.source_region(), PixelRect::new(256, 0, 44, 20));
    assert_eq!(unit.tile_region(), PixelRect::new(0, 0, 44, 20));
    assert_eq!(unit.codec().bits_per_sample(), 16);
    assert_eq!(unit.codec().byte_order(), Some(ByteOrder::LittleEndian));
    assert_eq!(assembled(unit), u16_le(&samples));

    let window = (0..20)
        .flat_map(|row| samples[row * 300 + 256..row * 300 + 300].to_vec())
        .collect::<Vec<_>>();
    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::None);
    assert_eq!(
        (raw.width(), raw.height(), raw.bits_allocated()),
        (44, 20, 16)
    );
    assert_eq!(raw.data(), u16_ne(&window));
    assert_eq!(
        slide.read_tile(&req).unwrap().data.as_u16(),
        Some(window.as_slice())
    );
}

#[test]
fn fragmented_stream_chains_become_ordered_segments() {
    for version in [cfb::Version::V3, cfb::Version::V4] {
        assert_fragmented_chain_segments(version);
    }
}

fn assert_fragmented_chain_segments(version: cfb::Version) {
    let samples = (0..200u16 * 20)
        .map(|value| value ^ 0x5a5a)
        .collect::<Vec<_>>();
    let spec = raw_u16_plane(200, 20, 0, &samples);
    let fixture = ZviFixture::write_with_version(
        &[spec.clone(), raw_u16_plane(200, 20, 1, &samples)],
        &whole_level_tags(200, 20),
        None,
        version,
    );
    let contents = plane_stream(&spec);
    let (first, rest) = contents.split_at(5_000);
    {
        let mut compound = cfb::open_rw(&fixture.path).unwrap();
        compound.remove_stream("/Image/Item(0)/Contents").unwrap();
        write_stream(&mut compound, "/Image/Item(0)/Contents", first);
        write_stream(&mut compound, "/Filler", &[0x33; 6_000]);
        let mut stream = compound.open_stream("/Image/Item(0)/Contents").unwrap();
        stream.seek(SeekFrom::End(0)).unwrap();
        stream.write_all(rest).unwrap();
        stream.flush().unwrap();
    }
    let slide = Slide::open(&fixture.path).unwrap();
    let req = plane_request(0, 0);

    let tile = stored(&slide, &req);
    let unit = tile.single_unit().unwrap();
    assert!(
        unit.segments().len() > 1,
        "the appended half is stored after the filler: {:?}",
        unit.segments()
    );
    assert!(unit
        .segments()
        .windows(2)
        .all(|pair| pair[0].end() != Some(pair[1].offset)));
    let bytes = assembled(unit);
    assert_eq!(bytes, u16_le(&samples));
    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.data(), u16_ne(&samples));
    assert_eq!(
        slide.read_tile(&req).unwrap().data.as_u16(),
        Some(samples.as_slice())
    );
}

#[test]
fn zlib_windows_are_exact_samples_and_jpeg_windows_are_cropped_losslessly() {
    let (width, height) = (300u32, 8u32);
    let samples = (0..width * height)
        .map(|value| (value % 251) as u8)
        .collect::<Vec<_>>();
    let jpeg = encode_gray_jpeg(width, height, &samples);
    let fixture = ZviFixture::write(
        &[
            PlaneSpec {
                encoding: PlaneEncoding::Zlib(samples.clone()),
                ..PlaneSpec::raw_u8(width, height, Vec::new())
            },
            PlaneSpec {
                c: 1,
                encoding: PlaneEncoding::Jpeg(jpeg.clone()),
                ..PlaneSpec::raw_u8(width, height, Vec::new())
            },
        ],
        &whole_level_tags(width, height),
        None,
    );
    let slide = Slide::open(&fixture.path).unwrap();

    let zlib = plane_request(1, 0);
    let raw = slide.read_raw_compressed_tile(&zlib).unwrap();
    assert_eq!(raw.compression(), Compression::None);
    assert_eq!((raw.width(), raw.height()), (44, 8));
    assert_eq!(Some(raw.data()), slide.read_tile(&zlib).unwrap().as_u8());
    let tile = stored(&slide, &zlib);
    let unit = tile.single_unit().unwrap();
    assert_eq!(unit.codec().compression(), Compression::Deflate);
    assert_eq!(unit.source_region(), PixelRect::new(256, 0, 44, 8));

    let jpeg_req = plane_request(1, 1);
    let raw = slide.read_raw_compressed_tile(&jpeg_req).unwrap();
    // x = 256 is on the 8-pixel grayscale MCU grid, so the crop is the tile.
    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!((raw.width(), raw.height()), (44, 8));
    assert_eq!(raw.tile_region(), PixelRect::new(0, 0, 44, 8));
    let tile = stored(&slide, &jpeg_req);
    let unit = tile.single_unit().unwrap();
    assert_eq!(assembled(unit), jpeg);
    assert_eq!(unit.source_region(), PixelRect::new(256, 0, 44, 8));
    let decode = |data: &[u8], width, height| {
        crate::core::batch::exactly_one(
            crate::decode::jpeg::decode_batch_jpeg(&[JpegDecodeJob::with_expected(
                data, width, height,
            )]),
            "ZVI JPEG",
        )
        .unwrap()
        .unwrap()
    };
    let whole = decode(&jpeg, width, height);
    let channels = whole.channels() as usize;
    let whole = whole.as_u8().unwrap();
    let expected = (0..height as usize)
        .flat_map(|row| {
            let start = (row * width as usize + 256) * channels;
            whole[start..start + 44 * channels].to_vec()
        })
        .collect::<Vec<_>>();
    assert_eq!(decode(raw.data(), 44, 8).as_u8(), Some(expected.as_slice()));
    assert_eq!(
        slide.read_tile(&jpeg_req).unwrap().as_u8(),
        Some(expected.as_slice())
    );
}

#[test]
fn mosaic_tiles_are_whole_planes_and_missing_channel_tiles_are_absent() {
    let tile = |tile_index: i32, c: u32, value: u8, x: &str| PlaneSpec {
        c,
        tile_index,
        tags: vec![(2073, x.into()), (2074, "0".into())],
        ..PlaneSpec::raw_u8(256, 2, vec![value; 512])
    };
    let fixture = ZviFixture::write(
        &[
            tile(1, 0, 17, "0"),
            tile(2, 0, 231, "256"),
            tile(1, 1, 99, "0"),
        ],
        &[
            (515, "512".into()),
            (516, "2".into()),
            (769, "1".into()),
            (772, "1".into()),
        ],
        None,
    );
    let slide = Slide::open(&fixture.path).unwrap();

    let first = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap();
    assert_eq!(first.tiles.len(), 2);
    for (entry, value) in first.tiles.iter().zip([17, 231]) {
        let TileLocation::Stored(stored) = &entry.location else {
            panic!("mosaic tile ({}, {}) is stored", entry.col, entry.row);
        };
        assert_eq!(stored.tile_size(), (256, 2));
        let bytes = assembled(stored.single_unit().unwrap());
        assert_eq!(bytes, vec![value; 512]);
        let raw = slide
            .read_raw_compressed_tile(&TileRequest::new(0, 0, 0, entry.col, entry.row))
            .unwrap();
        assert_eq!(raw.data(), bytes.as_slice());
    }

    let second = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::new(0, 1, 0))
        .unwrap();
    assert!(matches!(second.tiles[0].location, TileLocation::Stored(_)));
    assert_eq!(second.tiles[1].location, TileLocation::Absent);
    let missing = TileRequest::new(0, 0, 0, second.tiles[1].col, second.tiles[1].row)
        .with_plane(PlaneSelection::new(0, 1, 0));
    let error = slide.read_raw_compressed_tile(&missing).unwrap_err();
    assert!(
        matches!(&error, WsiError::Unsupported { reason } if reason.contains("Absent")),
        "{error}"
    );
}

#[test]
fn malformed_sector_chains_and_lengths_are_errors() {
    let samples = vec![0x1234u16; 300 * 20];
    let fixture = ZviFixture::write(
        &[raw_u16_plane(300, 20, 0, &samples)],
        &whole_level_tags(300, 20),
        None,
    );
    let path = "/Image/Item(0)/Contents";
    let stream_len = 94 + 12_000;
    let file = fs::File::open(&fixture.path).unwrap();
    let file_len = file.metadata().unwrap().len();
    let mut map = CfbSectorMap::read(&file, file_len, 1024, 1 << 20).unwrap();
    let ranges = map.stream_ranges(path, stream_len, 94, 12_000).unwrap();
    assert!(map
        .stream_ranges(path, stream_len + 1, 94, 12_000)
        .unwrap_err()
        .contains("reads as"));
    assert!(map
        .stream_ranges(path, stream_len, 94, 12_001)
        .unwrap_err()
        .contains("outside stream"));
    assert!(map
        .stream_ranges("/Image/Missing", 1, 0, 1)
        .unwrap_err()
        .contains("no stream"));
    assert!(CfbSectorMap::read(&file, file_len, 1, 1 << 20)
        .err()
        .is_some_and(|error| error.contains("entry limit")));
    drop(file);

    // Point the stream's first sector at itself in the FAT.
    let mut bytes = fs::read(&fixture.path).unwrap();
    let sector_len = 1u64 << u16::from_le_bytes([bytes[0x1e], bytes[0x1f]]);
    let first_sector = (ranges[0].offset / sector_len - 1) as u32;
    let fat_sector = u32::from_le_bytes(bytes[0x4c..0x50].try_into().unwrap());
    let entry = ((u64::from(fat_sector) + 1) * sector_len) as usize + first_sector as usize * 4;
    bytes[entry..entry + 4].copy_from_slice(&first_sector.to_le_bytes());
    let looped = fixture.path.with_file_name("looped.zvi");
    fs::write(&looped, &bytes).unwrap();
    let file = fs::File::open(&looped).unwrap();
    let mut map = CfbSectorMap::read(&file, bytes.len() as u64, 1024, 1 << 20).unwrap();
    assert!(map
        .stream_ranges(path, stream_len, 94, 12_000)
        .unwrap_err()
        .contains("repeated sector"));

    // A file cut inside the payload leaves sectors past its end.
    let cut = fixture.path.with_file_name("cut.zvi");
    fs::write(&cut, &bytes[..ranges.last().unwrap().offset as usize + 8]).unwrap();
    let file = fs::File::open(&cut).unwrap();
    let cut_len = file.metadata().unwrap().len();
    if let Ok(mut map) = CfbSectorMap::read(&file, cut_len, 1024, 1 << 20) {
        assert!(map.stream_ranges(path, stream_len, 94, 12_000).is_err());
    }
}

#[test]
fn deep_storage_chains_index_each_name_once_within_a_byte_budget() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("deep.cfb");
    let depth = 2_000u64;
    let storage = vec!["a"; depth as usize].join("/");
    let stream_path = format!("/{storage}/s");
    let payload = (0..5_000u32).map(|value| value as u8).collect::<Vec<_>>();
    {
        let mut compound = cfb::create(&path).unwrap();
        compound.create_storage_all(format!("/{storage}")).unwrap();
        compound
            .create_stream(&stream_path)
            .unwrap()
            .write_all(&payload)
            .unwrap();
    }
    let file = fs::File::open(&path).unwrap();
    let file_len = file.metadata().unwrap().len();

    // A budget linear in the entry count is enough: names are kept once,
    // not once per ancestor path.
    let mut map = CfbSectorMap::read(&file, file_len, 1 << 20, (depth + 64) * 256).unwrap();
    let ranges = map
        .stream_ranges(&stream_path, payload.len() as u64, 0, payload.len() as u64)
        .unwrap();
    let bytes = fs::read(&path).unwrap();
    let located = ranges
        .iter()
        .flat_map(|range| bytes[range.offset as usize..][..range.length as usize].to_vec())
        .collect::<Vec<_>>();
    assert_eq!(located, payload);
    assert!(map
        .stream_ranges(&format!("/{storage}"), 0, 0, 0)
        .unwrap_err()
        .contains("no stream"));

    let error = CfbSectorMap::read(&file, file_len, 1 << 20, 64 * 1024)
        .err()
        .expect("a budget below the directory size is an error");
    assert!(error.contains("byte limit"), "{error}");
}

/// A 300x8 grayscale JPEG plane in channel 0, whose tile (1, 0) is its
/// columns 256..300, and the same samples uncompressed in channel 1.
fn jpeg_plane_fixture() -> (ZviFixture, Vec<u8>) {
    let (width, height) = (300u32, 8u32);
    let samples = (0..width * height)
        .map(|value| (value % 251) as u8)
        .collect::<Vec<_>>();
    let jpeg = encode_gray_jpeg(width, height, &samples);
    let fixture = ZviFixture::write(
        &[
            PlaneSpec {
                encoding: PlaneEncoding::Jpeg(jpeg.clone()),
                ..PlaneSpec::raw_u8(width, height, Vec::new())
            },
            PlaneSpec {
                c: 1,
                ..PlaneSpec::raw_u8(width, height, samples)
            },
        ],
        &whole_level_tags(width, height),
        None,
    );
    (fixture, jpeg)
}

fn open_with_limits(fixture: &ZviFixture, limits: crate::SlideLimits) -> Slide {
    Slide::open_with_options(
        &fixture.path,
        crate::SlideOpenOptions::default().with_limits(limits),
    )
    .unwrap()
}

#[test]
fn jpeg_crops_over_the_lossless_crop_limit_return_the_whole_plane() {
    let (fixture, jpeg) = jpeg_plane_fixture();
    let req = plane_request(1, 0);
    // Leave one byte for coefficients after the payload, the marker segments
    // parsing copies out of it, and the worst-case output.
    let source_len = jpeg.len() as u64;
    let working_set = 2 * source_len
        + crate::decode::jpeg::lossless::lossless_crop_output_bound(source_len, 44, 8)
        + 1;
    let slide = open_with_limits(
        &fixture,
        crate::SlideLimits::default()
            .with_lossless_crop_bytes(working_set)
            .unwrap(),
    );
    let tile = stored(&slide, &req);
    let unit = tile.single_unit().unwrap();
    let raw = slide.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!(raw.data(), assembled(unit));
    assert_eq!(raw.data(), jpeg);
    assert_eq!((raw.width(), raw.height()), (300, 8));
    assert_eq!(raw.tile_region(), unit.source_region());
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Monochrome2
    );
    let decoded = crate::core::batch::exactly_one(
        crate::decode::jpeg::decode_batch_jpeg(&[JpegDecodeJob::with_expected(raw.data(), 300, 8)]),
        "ZVI JPEG",
    )
    .unwrap()
    .unwrap();
    let channels = decoded.channels() as usize;
    let decoded = decoded.as_u8().unwrap();
    let region = raw.tile_region();
    let expected = (0..region.height as usize)
        .flat_map(|row| {
            let start = (row * 300 + region.x as usize) * channels;
            decoded[start..start + region.width as usize * channels].to_vec()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        slide.read_tile(&req).unwrap().as_u8(),
        Some(expected.as_slice())
    );

    // Under the default limit the same tile is cropped.
    let raw = Slide::open(&fixture.path)
        .unwrap()
        .read_raw_compressed_tile(&req)
        .unwrap();
    assert_eq!((raw.width(), raw.height()), (44, 8));
}

#[test]
fn only_raw_reads_of_jpeg_crops_reserve_the_lossless_crop_working_set() {
    let (fixture, _) = jpeg_plane_fixture();
    let crop = plane_request(1, 0);
    let raw_window = plane_request(1, 1);
    let limits = crate::SlideLimits::default();
    let slide = Slide::open(&fixture.path).unwrap();
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
            .raw_compressed_tile_reservation_hint(&raw_window)
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
    slide
        .read_raw_compressed_tile(&raw_window)
        .expect("raw reads that crop no JPEG reserve one encoded unit");
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
