use super::*;
use crate::core::registry::ManagedSlideReader;

pub(super) fn tile_location(reader: &TiffPixelReader, req: &TileRequest) -> TileLocation {
    reader
        .tile_location(req)
        .unwrap_or_else(|err| panic!("tile location of {req:?}: {err}"))
}

pub(super) fn stored_tile(location: &TileLocation) -> &StoredTile {
    match location {
        TileLocation::Stored(stored) => stored,
        other => panic!("expected a stored tile, got {other:?}"),
    }
}

pub(super) fn single_unit(location: &TileLocation) -> &EncodedUnit {
    stored_tile(location)
        .single_unit()
        .expect("tile is stored in one unit")
}

/// The standalone stream of `unit`, from its segments read out of the file.
pub(super) fn assemble_unit(reader: &TiffPixelReader, unit: &EncodedUnit) -> Vec<u8> {
    let segments: Vec<Vec<u8>> = unit
        .segments()
        .iter()
        .map(|segment| {
            reader
                .container
                .pread(segment.offset, segment.length)
                .unwrap()
        })
        .collect();
    unit.assemble(segments.iter().map(Vec::as_slice)).unwrap()
}

/// Asserts that `req`'s single unit assembles to the raw compressed tile.
pub(super) fn assert_location_round_trip(
    reader: &TiffPixelReader,
    req: &TileRequest,
) -> (EncodedUnit, RawCompressedTile) {
    let location = tile_location(reader, req);
    let unit = single_unit(&location).clone();
    let raw = reader
        .read_raw_compressed_tile(req)
        .unwrap_or_else(|err| panic!("raw tile {req:?}: {err}"));
    assert_eq!(
        assemble_unit(reader, &unit),
        raw.data(),
        "assembled location of {req:?} differs from the raw tile"
    );
    (unit, raw)
}

fn request(col: i64, row: i64) -> TileRequest {
    TileRequest::new(0usize, 0usize, 0u32, col, row)
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), DeflateCompression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn inflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut decoded = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .read_to_end(&mut decoded)
        .unwrap();
    decoded
}

/// A one-IFD 8-bit tiled TIFF.
struct TiledTiffSpec {
    width: u32,
    height: u32,
    tile: (u32, u32),
    compression: u16,
    samples: u16,
    photometric: u16,
    extra_tags: Vec<(u16, u16, u32, [u8; 4])>,
    /// Replaces the TileByteCounts values, for malformed files.
    byte_counts: Option<Vec<u32>>,
}

impl TiledTiffSpec {
    fn new(width: u32, height: u32, tile: (u32, u32), compression: u16) -> Self {
        Self {
            width,
            height,
            tile,
            compression,
            samples: 1,
            photometric: 1,
            extra_tags: Vec::new(),
            byte_counts: None,
        }
    }

    fn rgb(mut self) -> Self {
        self.samples = 3;
        self.photometric = 2;
        self
    }

    fn tag(mut self, tag: u16, value: u16) -> Self {
        self.extra_tags.push((tag, 3, 1, short_in_u32(value)));
        self
    }

    fn write(&self, tiles: &[Vec<u8>]) -> NamedTempFile {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&le_u16(42));
        let first_ifd_pos = buf.len();
        buf.extend_from_slice(&le_u32(0));
        let mut offsets = Vec::new();
        let mut byte_counts = Vec::new();
        for tile in tiles {
            offsets.push(buf.len() as u32);
            byte_counts.push(tile.len() as u32);
            buf.extend_from_slice(tile);
        }
        let byte_counts = self.byte_counts.clone().unwrap_or(byte_counts);
        let offsets_array = append_optional_u32_array(&mut buf, &offsets);
        let byte_counts_array = append_optional_u32_array(&mut buf, &byte_counts);
        let ifd_offset = buf.len() as u32;
        buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&le_u32(ifd_offset));
        let mut tags = vec![
            (256u16, 4u16, 1u32, le_u32(self.width)),
            (257, 4, 1, le_u32(self.height)),
            (258, 3, 1, short_in_u32(8)),
            (259, 3, 1, short_in_u32(self.compression)),
            (262, 3, 1, short_in_u32(self.photometric)),
            (277, 3, 1, short_in_u32(self.samples)),
            (322, 4, 1, le_u32(self.tile.0)),
            (323, 4, 1, le_u32(self.tile.1)),
            (
                324,
                4,
                offsets.len() as u32,
                u32_array_offset_or_inline_value(&offsets, offsets_array),
            ),
            (
                325,
                4,
                byte_counts.len() as u32,
                u32_array_offset_or_inline_value(&byte_counts, byte_counts_array),
            ),
        ];
        tags.extend(self.extra_tags.iter().copied());
        append_ifd_tags(&mut buf, tags);
        temp_tiff_from_buffer(&buf)
    }

    fn reader(&self, file: &NamedTempFile, compression: Compression) -> TiffPixelReader {
        let container = Arc::new(TiffContainer::open(file.path()).unwrap());
        let ifd_id = container.top_ifds()[0];
        let layout = single_series_layout(
            DatasetId::new(51),
            vec![regular_level(
                self.width,
                self.height,
                self.tile.0,
                self.tile.1,
            )],
            HashMap::from([(
                tile_source_key(0),
                TileSource::TiledIfd {
                    ifd_id,
                    jpeg_tables: None,
                    compression,
                },
            )]),
        );
        TiffPixelReader::new(container, layout)
    }
}

fn gray_ramp(width: u32, height: u32, seed: u8) -> Vec<u8> {
    (0..width * height)
        .map(|index| (index as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

#[test]
fn tiled_jpeg_locations_assemble_to_the_raw_tiles() {
    let tiles = [
        encode_solid_rgb_jpeg(8, 8, [200, 10, 30]),
        encode_solid_rgb_jpeg(8, 8, [10, 200, 30]),
    ];
    let reader = build_tiled_encoded_reader(16, 8, 8, 8, &tiles, Compression::Jpeg, 7, 3, 6);

    for (col, tile) in tiles.iter().enumerate() {
        let (unit, raw) = assert_location_round_trip(&reader, &request(col as i64, 0));
        assert_eq!(raw.data(), tile.as_slice());
        assert_eq!(unit.framing(), StoredFraming::Standalone);
        assert_eq!(unit.codec().compression(), Compression::Jpeg);
        assert_eq!(unit.codec().samples_per_pixel(), 3);
        assert_eq!(unit.codec().bits_per_sample(), 8);
        assert_eq!(unit.stored_size(), (8, 8));
        assert_eq!(unit.path(), reader.container.path());
    }
}

#[test]
fn tiled_jpeg_tables_locations_share_the_tables_and_assemble_to_the_raw_tiles() {
    let (first, tables) = split_test_jpeg_tables(&encode_solid_rgb_jpeg(8, 8, [40, 180, 90]));
    let (second, _) = split_test_jpeg_tables(&encode_solid_rgb_jpeg(8, 8, [180, 40, 90]));
    let reader = build_tiled_jpeg_reader_with_tables(16, 8, 8, 8, &[first, second], tables.clone());

    let units: Vec<EncodedUnit> = (0..2)
        .map(|col| assert_location_round_trip(&reader, &request(col, 0)).0)
        .collect();
    for unit in &units {
        assert_eq!(unit.framing(), StoredFraming::JpegTables);
        assert_eq!(unit.codec().jpeg_tables(), Some(tables.as_slice()));
        assert_eq!(
            unit.codec().photometric_interpretation(),
            EncodedTilePhotometricInterpretation::YbrFull422
        );
    }
    assert_eq!(
        units[0].codec().jpeg_tables().unwrap().as_ptr(),
        units[1].codec().jpeg_tables().unwrap().as_ptr(),
        "tiles of one IFD share one table buffer"
    );
}

#[test]
fn deflate_tiles_pass_through_as_stored_with_clipped_edge_regions() {
    let pixels = [gray_ramp(8, 8, 3), gray_ramp(8, 8, 90)];
    let spec = TiledTiffSpec::new(12, 8, (8, 8), 8);
    let file = spec.write(&[deflate(&pixels[0]), deflate(&pixels[1])]);
    let reader = spec.reader(&file, Compression::Deflate);

    for col in 0..2 {
        let req = request(col, 0);
        let (unit, raw) = assert_location_round_trip(&reader, &req);
        assert_eq!(unit.framing(), StoredFraming::BarePayload);
        assert_eq!(unit.codec().compression(), Compression::Deflate);
        assert_eq!(unit.codec().predictor(), Predictor::None);
        assert_eq!(unit.codec().byte_order(), Some(ByteOrder::LittleEndian));
        assert_eq!(unit.stored_size(), (8, 8));
        assert_eq!(raw.compression(), Compression::Deflate);
        assert_eq!((raw.width(), raw.height()), (8, 8));
        assert_eq!(inflate(raw.data()), pixels[col as usize]);

        let tile = reader.read_tile_cpu(&req).unwrap();
        let width = if col == 0 { 8 } else { 4 };
        let clipped = PixelRect::new(0, 0, width, 8);
        assert_eq!(
            stored_tile(&tile_location(&reader, &req)).tile_size(),
            (width, 8)
        );
        assert_eq!(unit.source_region(), clipped);
        assert_eq!(unit.tile_region(), clipped);
        assert_eq!(raw.tile_region(), clipped);
        let visible: Vec<u8> = pixels[col as usize]
            .chunks(8)
            .flat_map(|row| row[..width as usize].to_vec())
            .collect();
        assert_eq!(tile.data.as_u8(), Some(visible.as_slice()));
    }
}

#[test]
fn uncompressed_rgb_tiles_pass_through_as_the_decoded_samples() {
    let pixels: Vec<u8> = (0..8 * 8 * 3).map(|value| value as u8).collect();
    let spec = TiledTiffSpec::new(8, 8, (8, 8), 1).rgb();
    let file = spec.write(std::slice::from_ref(&pixels));
    let reader = spec.reader(&file, Compression::None);

    let (unit, raw) = assert_location_round_trip(&reader, &request(0, 0));
    assert_eq!(unit.framing(), StoredFraming::BarePayload);
    assert_eq!(raw.compression(), Compression::None);
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Rgb
    );
    assert_eq!((raw.bits_allocated(), raw.samples_per_pixel()), (8, 3));
    assert_eq!(raw.data(), pixels.as_slice());
    assert_eq!(
        reader.read_tile_cpu(&request(0, 0)).unwrap().data.as_u8(),
        Some(pixels.as_slice())
    );
}

#[test]
fn predictor_tiles_report_the_predictor_and_pass_through_decoded() {
    let pixels = gray_ramp(8, 8, 11);
    let differenced: Vec<u8> = pixels
        .chunks(8)
        .flat_map(|row| {
            (0..8).map(move |x| {
                if x == 0 {
                    row[0]
                } else {
                    row[x].wrapping_sub(row[x - 1])
                }
            })
        })
        .collect();
    let spec = TiledTiffSpec::new(8, 8, (8, 8), 8).tag(317, 2);
    let file = spec.write(&[deflate(&differenced)]);
    let reader = spec.reader(&file, Compression::Deflate);
    let req = request(0, 0);

    let location = tile_location(&reader, &req);
    let unit = single_unit(&location);
    assert_eq!(unit.codec().predictor(), Predictor::Horizontal);
    assert_eq!(inflate(&assemble_unit(&reader, unit)), differenced);

    let raw = reader.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::None);
    assert_eq!((raw.width(), raw.height()), (8, 8));
    assert_eq!(raw.data(), pixels.as_slice());
    assert!(reader.tile_encoded_upper_bound(&req).unwrap() >= raw.data().len() as u64);
}

#[test]
fn empty_tiles_are_absent_and_have_no_raw_payload() {
    let spec = TiledTiffSpec::new(16, 8, (8, 8), 8);
    let file = spec.write(&[deflate(&gray_ramp(8, 8, 1)), Vec::new()]);
    let reader = spec.reader(&file, Compression::Deflate);

    assert!(matches!(
        tile_location(&reader, &request(0, 0)),
        TileLocation::Stored(_)
    ));
    assert_eq!(tile_location(&reader, &request(1, 0)), TileLocation::Absent);
    let err = reader.read_raw_compressed_tile(&request(1, 0)).unwrap_err();
    assert!(
        err.to_string().contains("Absent"),
        "unexpected error: {err}"
    );
}

#[test]
fn planar_tiles_list_one_unit_per_sample_plane_and_interleave_raw_samples() {
    let planes: Vec<Vec<u8>> = (0..3).map(|plane| vec![plane * 40; 64]).collect();
    let spec = TiledTiffSpec::new(8, 8, (8, 8), 1).rgb().tag(284, 2);
    let file = spec.write(&planes);
    let reader = spec.reader(&file, Compression::None);
    let req = request(0, 0);

    let location = tile_location(&reader, &req);
    let units = stored_tile(&location).units();
    assert_eq!(units.len(), 3);
    for (unit, plane) in units.iter().zip(&planes) {
        assert_eq!(
            unit.codec().planar_configuration(),
            PlanarConfiguration::Planar
        );
        assert_eq!(unit.tile_region(), PixelRect::new(0, 0, 8, 8));
        assert_eq!(&assemble_unit(&reader, unit), plane);
    }

    // Raw reads interleave the planes into the tile's exact samples.
    let raw = reader.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.compression(), Compression::None);
    assert_eq!((raw.width(), raw.height()), (8, 8));
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Rgb
    );
    assert_eq!(raw.data(), [0, 40, 80].repeat(64).as_slice());
    assert!(reader.tile_encoded_upper_bound(&req).unwrap() >= raw.data().len() as u64);
}

#[test]
fn planar_predictor_tiles_undo_the_predictor_per_plane() {
    let planes: Vec<Vec<u8>> = (0..3u8).map(|plane| gray_ramp(8, 8, plane * 50)).collect();
    let differenced: Vec<Vec<u8>> = planes
        .iter()
        .map(|plane| {
            let rows = plane.chunks(8).flat_map(|row| {
                (0..8).map(move |x| {
                    if x == 0 {
                        row[0]
                    } else {
                        row[x].wrapping_sub(row[x - 1])
                    }
                })
            });
            deflate(&rows.collect::<Vec<u8>>())
        })
        .collect();
    let spec = TiledTiffSpec::new(8, 8, (8, 8), 8)
        .rgb()
        .tag(284, 2)
        .tag(317, 2);
    let file = spec.write(&differenced);
    let reader = spec.reader(&file, Compression::Deflate);

    let raw = reader.read_raw_compressed_tile(&request(0, 0)).unwrap();
    let interleaved: Vec<u8> = (0..64)
        .flat_map(|pixel| planes.iter().map(move |plane| plane[pixel]))
        .collect();
    assert_eq!(raw.data(), interleaved.as_slice());

    // Floating-point differencing has no sample decoder here.
    let spec = TiledTiffSpec::new(8, 8, (8, 8), 8)
        .rgb()
        .tag(284, 2)
        .tag(317, 3);
    let file = spec.write(&differenced);
    let reader = spec.reader(&file, Compression::Deflate);
    let err = reader.read_raw_compressed_tile(&request(0, 0)).unwrap_err();
    assert!(
        err.to_string().contains("horizontal differencing"),
        "unexpected error: {err}"
    );
}

#[test]
fn jpegxr_tiles_locate_complete_streams() {
    let encoded = include_bytes!("../../../../../tests/fixtures/jxr/rgb.jxr").to_vec();
    let reader = build_tiled_encoded_reader(
        19,
        16,
        16,
        16,
        &[encoded.clone(), encoded.clone()],
        Compression::JpegXr,
        22610,
        3,
        2,
    );

    for col in 0..2 {
        let (unit, raw) = assert_location_round_trip(&reader, &request(col, 0));
        assert_eq!(unit.framing(), StoredFraming::Standalone);
        assert_eq!(raw.compression(), Compression::JpegXr);
        assert_eq!(raw.data(), encoded.as_slice());
        assert_eq!(unit.stored_size(), (16, 16));
    }
    let edge = tile_location(&reader, &request(1, 0));
    assert_eq!(
        single_unit(&edge).source_region(),
        PixelRect::new(0, 0, 3, 16)
    );
}

#[test]
fn jp2k_tiles_locate_their_codestream() {
    let codestream = include_bytes!("../../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let expected = load_fixture_rgb(include_bytes!(
        "../../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));
    let reader = build_tiled_encoded_reader(
        expected.width(),
        expected.height(),
        expected.width(),
        expected.height(),
        std::slice::from_ref(&codestream),
        Compression::Jp2kRgb,
        33004,
        3,
        2,
    );

    let (unit, raw) = assert_location_round_trip(&reader, &request(0, 0));
    assert_eq!(unit.framing(), StoredFraming::Standalone);
    assert_eq!(
        unit.codec().photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Rgb
    );
    assert_eq!(raw.data(), codestream.as_slice());
}

#[test]
fn malformed_tile_ranges_fail_without_panicking() {
    let tile = deflate(&gray_ramp(8, 8, 5));
    for byte_counts in [vec![u32::MAX], vec![tile.len() as u32 + 4096]] {
        let mut spec = TiledTiffSpec::new(8, 8, (8, 8), 8);
        spec.byte_counts = Some(byte_counts);
        let file = spec.write(std::slice::from_ref(&tile));
        let reader = spec.reader(&file, Compression::Deflate);

        let err = reader.tile_location(&request(0, 0)).unwrap_err();
        assert!(
            err.to_string().contains("exceeds"),
            "unexpected error: {err}"
        );
    }

    let spec = TiledTiffSpec::new(8, 8, (8, 8), 8);
    let file = spec.write(std::slice::from_ref(&tile));
    let mut reader = spec.reader(&file, Compression::Deflate);
    reader.layout.dataset.scenes[0].series[0].levels[0].dimensions = (16, 8);
    reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::Regular {
        tile_width: 8,
        tile_height: 8,
        tiles_across: 2,
        tiles_down: 1,
    };
    let err = reader.tile_location(&request(1, 0)).unwrap_err();
    assert!(
        err.to_string().contains("out of range"),
        "unexpected error: {err}"
    );
}

#[test]
fn synthetic_levels_are_not_addressable() {
    let mut reader = build_tiled_jpeg_reader(8, 8, 8, 8, &[encode_solid_rgb_jpeg(8, 8, [1, 2, 3])]);
    reader.layout.tile_sources.insert(
        tile_source_key(0),
        TileSource::SyntheticDownsample {
            base_level: 0,
            factor: 2,
        },
    );

    assert_eq!(
        tile_location(&reader, &request(0, 0)),
        TileLocation::NotAddressable(NotAddressableReason::SyntheticLevel {
            base_level: LevelIdx::new(0),
            factor: 2,
        })
    );
    let err = reader.read_raw_compressed_tile(&request(0, 0)).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("synthesized") && message.contains("tile_location"),
        "unexpected error: {message}"
    );
}

#[test]
fn level_tile_locations_list_every_grid_cell_of_a_slide() {
    let tiles = [
        encode_solid_rgb_jpeg(8, 8, [200, 10, 10]),
        Vec::new(),
        encode_solid_rgb_jpeg(8, 8, [10, 200, 10]),
        encode_solid_rgb_jpeg(8, 8, [10, 10, 200]),
    ];
    let file = build_tiled_encoded_tiff(16, 16, 8, 8, &tiles, 7, 3, 6);
    let slide = crate::Slide::open(file.path()).unwrap();

    let level = slide
        .level_tile_locations(0usize, 0usize, 0u32, PlaneSelection::default())
        .unwrap();
    assert_eq!((level.tile_width, level.tile_height), (8, 8));
    assert_eq!((level.tiles_across, level.tiles_down), (2, 2));
    let cells: Vec<(i64, i64)> = level
        .tiles
        .iter()
        .map(|tile| (tile.col, tile.row))
        .collect();
    assert_eq!(cells, [(0, 0), (1, 0), (0, 1), (1, 1)]);
    assert_eq!(level.tiles[1].location, TileLocation::Absent);

    let file_bytes = std::fs::read(file.path()).unwrap();
    for (entry, tile) in level.tiles.iter().zip(&tiles) {
        let TileLocation::Stored(stored) = &entry.location else {
            continue;
        };
        let unit = stored.single_unit().unwrap();
        assert_eq!(unit.path(), file.path());
        let segments: Vec<&[u8]> = unit
            .segments()
            .iter()
            .map(|segment| {
                &file_bytes[segment.offset as usize..(segment.offset + segment.length) as usize]
            })
            .collect();
        let assembled = unit.assemble(segments).unwrap();
        assert_eq!(&assembled, tile);
        let req = TileRequest::new(0usize, 0usize, 0u32, entry.col, entry.row);
        assert_eq!(slide.read_raw_compressed_tile(&req).unwrap().data(), tile);
        assert_eq!(slide.tile_location(&req).unwrap(), entry.location);
    }
}

#[test]
fn ventana_subtile_locations_select_the_stored_tile_share() {
    let stored = [
        encode_solid_rgb_jpeg(8, 8, [200, 10, 10]),
        encode_solid_rgb_jpeg(8, 8, [10, 200, 10]),
    ];
    let mut reader = build_tiled_jpeg_reader(16, 8, 8, 8, &stored);
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
    let mut tiles: HashMap<(i64, i64), TileEntry> = (0..4)
        .flat_map(|col| (0..2).map(move |row| ((col, row), TileEntry::new((0.0, 0.0), (4, 4)))))
        .collect();
    tiles.insert(
        (5, 0),
        TileEntry::new((0.0, 0.0), (4, 4)).with_extent((3.5, 3.5)),
    );
    reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::Irregular {
        tile_advance: (4.0, 4.0),
        extra_tiles: (0, 0, 0, 0),
        tiles,
    };

    let location = tile_location(&reader, &request(3, 1));
    let unit = single_unit(&location);
    assert_eq!(stored_tile(&location).tile_size(), (4, 4));
    assert_eq!(unit.source_region(), PixelRect::new(4, 4, 4, 4));
    assert_eq!(unit.tile_region(), PixelRect::new(0, 0, 4, 4));
    assert_eq!(unit.stored_size(), (8, 8));
    assert_eq!(assemble_unit(&reader, unit), stored[1]);

    assert_eq!(
        tile_location(&reader, &request(5, 0)),
        TileLocation::NotAddressable(NotAddressableReason::Resampled)
    );
}

/// A 300 x 4 Deflate grayscale TIFF in two 2-row strips.
fn stripped_level_fixture() -> (NamedTempFile, Vec<Vec<u8>>) {
    let rows: Vec<Vec<u8>> = (0..2u8)
        .map(|strip| {
            (0..600u32)
                .map(|index| (index as u8).wrapping_add(strip * 70))
                .collect()
        })
        .collect();
    let strips: Vec<Vec<u8>> = rows.iter().map(|raw| deflate(raw)).collect();
    (build_multi_stripped_tiff(300, 4, 2, &strips, 8, 1, 1), rows)
}

#[test]
fn stripped_level_locations_list_each_strip_a_tile_covers() {
    let (file, rows) = stripped_level_fixture();
    let slide = crate::Slide::open(file.path()).unwrap();
    let reader = slide.source();

    for (col, x, width) in [(0, 0, 256), (1, 256, 44)] {
        let req = request(col, 0);
        let location = reader.tile_location(&req).unwrap();
        let stored = stored_tile(&location);
        assert_eq!(stored.tile_size(), (width, 4));
        assert_eq!(stored.units().len(), 2);
        for (strip, unit) in stored.units().iter().enumerate() {
            assert_eq!(unit.framing(), StoredFraming::BarePayload);
            assert_eq!(unit.stored_size(), (300, 2));
            assert_eq!(unit.source_region(), PixelRect::new(x, 0, width, 2));
            assert_eq!(
                unit.tile_region(),
                PixelRect::new(0, 2 * strip as u32, width, 2)
            );
            let bytes = std::fs::read(file.path()).unwrap();
            let segment = unit.segments()[0];
            let stream = unit
                .assemble([
                    &bytes[segment.offset as usize..(segment.offset + segment.length) as usize]
                ])
                .unwrap();
            assert_eq!(inflate(&stream), rows[strip]);
        }

        let raw = slide.read_raw_compressed_tile(&req).unwrap();
        assert_eq!(raw.compression(), Compression::None);
        assert_eq!((raw.width(), raw.height()), (width, 4));
        let tile = slide.read_tile(&req).unwrap();
        assert_eq!(tile.data.as_u8(), Some(raw.data()));
    }
}

#[test]
fn rgb_tagged_subsampled_jpeg_locations_follow_the_tile_color_markers() {
    // Photometric says RGB, but the tile's chroma subsampling marks YCbCr,
    // which raw reads and decoding follow.
    let image = image::RgbImage::from_fn(16, 16, |x, y| image::Rgb([x as u8 * 9, y as u8 * 9, 90]));
    let mut jpeg = Vec::new();
    let mut encoder = JpegEncoder::new(&mut jpeg, 90);
    encoder.set_sampling_factor(JpegSamplingFactor::R_4_2_0);
    encoder
        .encode(image.as_raw(), 16, 16, JpegColorType::Rgb)
        .unwrap();
    let reader = build_tiled_encoded_reader(
        16,
        16,
        16,
        16,
        std::slice::from_ref(&jpeg),
        Compression::Jpeg,
        7,
        3,
        2,
    );

    let (unit, raw) = assert_location_round_trip(&reader, &request(0, 0));
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::YbrFull422
    );
    assert_eq!(
        unit.codec().photometric_interpretation(),
        raw.photometric_interpretation()
    );
}

/// A 16 x 16 RGB-encoded JPEG tile with `sampling`.
fn jpeg_tile(seed: u8, sampling: JpegSamplingFactor) -> Vec<u8> {
    let image = image::RgbImage::from_fn(16, 16, |x, y| {
        image::Rgb([x as u8 * 9, y as u8 * 9, seed.wrapping_add((x + y) as u8)])
    });
    let mut jpeg = Vec::new();
    let mut encoder = JpegEncoder::new(&mut jpeg, 90);
    encoder.set_sampling_factor(sampling);
    encoder
        .encode(image.as_raw(), 16, 16, JpegColorType::Rgb)
        .unwrap();
    jpeg
}

/// `jpeg` with `segment` inserted right after SOI.
fn with_leading_segment(jpeg: &[u8], segment: &[u8]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(segment);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// An APPn segment of the largest length a marker segment can state.
fn max_length_app_segment(marker: u8) -> Vec<u8> {
    let mut segment = vec![0xFF, marker, 0xFF, 0xFF];
    segment.resize(2 + 0xFFFF, 0);
    segment
}

#[test]
fn rgb_tagged_jpeg_tiles_each_follow_their_own_color_markers() {
    use EncodedTilePhotometricInterpretation::{Rgb, YbrFull422};
    let adobe_ycbcr = [
        0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 1,
    ];
    let tiles = [
        // Subsampled chroma marks YCbCr.
        (jpeg_tile(10, JpegSamplingFactor::R_4_2_0), YbrFull422),
        // Unsubsampled, unmarked components follow the RGB tag.
        (jpeg_tile(20, JpegSamplingFactor::R_4_4_4), Rgb),
        // An Adobe marker states YCbCr for unsubsampled components.
        (
            with_leading_segment(&jpeg_tile(30, JpegSamplingFactor::R_4_4_4), &adobe_ycbcr),
            YbrFull422,
        ),
        // A first tile's interpretation is not reused for later tiles.
        (jpeg_tile(40, JpegSamplingFactor::R_4_4_4), Rgb),
    ];
    let stored: Vec<Vec<u8>> = tiles.iter().map(|(jpeg, _)| jpeg.clone()).collect();
    let reader = build_tiled_encoded_reader(64, 16, 16, 16, &stored, Compression::Jpeg, 7, 3, 2);

    for (col, (_, expected)) in tiles.iter().enumerate() {
        let (unit, raw) = assert_location_round_trip(&reader, &request(col as i64, 0));
        assert_eq!(
            unit.codec().photometric_interpretation(),
            *expected,
            "tile {col} location"
        );
        assert_eq!(
            raw.photometric_interpretation(),
            *expected,
            "tile {col} raw"
        );
    }
}

#[test]
fn jpeg_color_markers_after_a_max_length_app_segment_still_decide() {
    // Two 64 KiB APP segments put the SOF past any fixed header window.
    let padding = [max_length_app_segment(0xE1), max_length_app_segment(0xE2)].concat();
    let tile = with_leading_segment(&jpeg_tile(50, JpegSamplingFactor::R_4_2_0), &padding);
    let reader = build_tiled_encoded_reader(
        16,
        16,
        16,
        16,
        std::slice::from_ref(&tile),
        Compression::Jpeg,
        7,
        3,
        2,
    );

    let (unit, raw) = assert_location_round_trip(&reader, &request(0, 0));
    assert_eq!(
        unit.codec().photometric_interpretation(),
        EncodedTilePhotometricInterpretation::YbrFull422
    );
    assert_eq!(
        raw.photometric_interpretation(),
        unit.codec().photometric_interpretation()
    );
}

#[test]
fn jpeg_tiles_whose_color_markers_are_out_of_reach_fail_their_location() {
    // The APP segment states more bytes than the tile holds, so its SOF
    // cannot be reached.
    let jpeg = jpeg_tile(60, JpegSamplingFactor::R_4_4_4);
    let mut truncated = with_leading_segment(&jpeg, &[0xFF, 0xE1, 0xFF, 0xFF]);
    truncated.truncate(jpeg.len());
    let tiles = [jpeg_tile(70, JpegSamplingFactor::R_4_4_4), truncated];

    let reader = build_tiled_encoded_reader(32, 16, 16, 16, &tiles, Compression::Jpeg, 7, 3, 2);
    assert_eq!(
        single_unit(&tile_location(&reader, &request(0, 0)))
            .codec()
            .photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Rgb
    );
    let err = reader.tile_location(&request(1, 0)).unwrap_err();
    assert!(
        err.to_string().contains("color markers cannot be read"),
        "unexpected error: {err}"
    );

    // A YCbCr tag decides every tile's color, so no tile is read.
    let reader = build_tiled_encoded_reader(32, 16, 16, 16, &tiles, Compression::Jpeg, 7, 3, 6);
    assert_eq!(
        single_unit(&tile_location(&reader, &request(1, 0)))
            .codec()
            .photometric_interpretation(),
        EncodedTilePhotometricInterpretation::YbrFull422
    );
}

#[test]
fn unsubsampled_ycbcr_payloads_pass_through_and_subsampled_ones_are_refused() {
    let samples: Vec<u8> = (0..8 * 8 * 3).map(|value| value as u8).collect();
    let ycbcr = |spec: TiledTiffSpec| {
        let mut spec = spec;
        spec.samples = 3;
        spec.photometric = 6;
        spec
    };
    let mut unsubsampled = ycbcr(TiledTiffSpec::new(8, 8, (8, 8), 1));
    unsubsampled.extra_tags.push((530, 3, 2, [1, 0, 1, 0]));
    let file = unsubsampled.write(std::slice::from_ref(&samples));
    let reader = unsubsampled.reader(&file, Compression::None);
    let (unit, raw) = assert_location_round_trip(&reader, &request(0, 0));
    assert_eq!(unit.codec().chroma_subsampling(), Some((1, 1)));
    assert_eq!(raw.compression(), Compression::None);
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::YbrFull422
    );
    assert_eq!(raw.data(), samples.as_slice());

    // Without the tag TIFF packs YCbCr samples 2x2 subsampled.
    let subsampled = ycbcr(TiledTiffSpec::new(8, 8, (8, 8), 1));
    let file = subsampled.write(std::slice::from_ref(&samples));
    let reader = subsampled.reader(&file, Compression::None);
    let location = tile_location(&reader, &request(0, 0));
    assert_eq!(
        single_unit(&location).codec().chroma_subsampling(),
        Some((2, 2))
    );
    let err = reader.read_raw_compressed_tile(&request(0, 0)).unwrap_err();
    assert!(
        err.to_string().contains("subsampled"),
        "unexpected error: {err}"
    );
}

#[test]
fn whole_bare_strips_pass_through_as_stored() {
    let rows: Vec<Vec<u8>> = (0..2u8)
        .map(|strip| {
            (0..200 * 16u32)
                .map(|index| (index as u8) ^ (strip * 99))
                .collect()
        })
        .collect();
    let strips: Vec<Vec<u8>> = rows.iter().map(|raw| deflate(raw)).collect();
    let file = build_multi_stripped_tiff(200, 32, 16, &strips, 8, 1, 1);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = container.top_ifds()[0];
    let layout = single_series_layout(
        DatasetId::new(52),
        vec![regular_level(200, 32, 200, 16)],
        HashMap::from([(
            tile_source_key(0),
            TileSource::StrippedLevel {
                ifd_id,
                compression: Compression::Deflate,
                jpeg_tables: None,
                rows_per_strip: 16,
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
    let reader = TiffPixelReader::new(container, layout);

    // Each 200 x 16 tile is exactly one strip.
    let (_, raw) = assert_location_round_trip(&reader, &request(0, 1));
    assert_eq!(raw.compression(), Compression::Deflate);
    assert_eq!((raw.width(), raw.height()), (200, 16));
    assert_eq!(raw.data(), strips[1].as_slice());
    assert_eq!(inflate(raw.data()), rows[1]);
}

#[test]
fn decoded_raw_fallback_admission_covers_the_physical_stored_tile() {
    // A 1 x 1 image in one 64 x 64 stored tile: decoding inflates the whole
    // stored tile although the raw tile is a single sample.
    let stored = gray_ramp(64, 64, 9);
    let differenced: Vec<u8> = stored
        .chunks(64)
        .flat_map(|row| {
            (0..64).map(move |x| {
                if x == 0 {
                    row[0]
                } else {
                    row[x].wrapping_sub(row[x - 1])
                }
            })
        })
        .collect();
    let spec = TiledTiffSpec::new(1, 1, (64, 64), 8).tag(317, 2);
    let file = spec.write(&[deflate(&differenced)]);
    let reader = spec.reader(&file, Compression::Deflate);
    let req = request(0, 0);

    let raw = reader.read_raw_compressed_tile(&req).unwrap();
    assert_eq!(raw.data(), &stored[..1]);
    assert!(
        reader.tile_encoded_upper_bound(&req).unwrap() >= 2 * 64 * 64,
        "admission must cover the inflated stored tile and its sample copy"
    );
}
