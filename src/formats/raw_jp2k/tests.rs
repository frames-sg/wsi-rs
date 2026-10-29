use std::fs;

use tempfile::TempDir;

use super::*;

const RGB_CODESTREAM: &[u8] = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");

struct RawJp2kFixture {
    _temp: TempDir,
    path: std::path::PathBuf,
}

fn write_fixture(extension: &str, bytes: &[u8]) -> RawJp2kFixture {
    let temp = tempfile::tempdir().expect("temporary raw JP2K directory");
    let path = temp.path().join(format!("synthetic.{extension}"));
    fs::write(&path, bytes).expect("write raw JP2K fixture");
    RawJp2kFixture { _temp: temp, path }
}

fn expect_error<T>(result: Result<T, WsiError>, context: &str) -> WsiError {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

#[test]
fn probe_requires_supported_extension_and_soc_marker() {
    let backend = RawJp2kBackend;
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let detected = backend
        .probe(&fixture.path)
        .expect("probe valid codestream");
    assert!(detected.detected);
    assert_eq!(detected.vendor, "raw-jp2k");
    assert_eq!(detected.confidence, ProbeConfidence::Definite);

    let upper = fixture.path.with_extension("J2C");
    fs::rename(&fixture.path, &upper).expect("rename to uppercase J2C");
    assert!(backend.probe(&upper).expect("probe uppercase J2C").detected);

    for path in [upper.with_extension("jp2"), upper.with_extension("")] {
        fs::write(&path, RGB_CODESTREAM).expect("write unsupported extension fixture");
        assert!(
            !backend
                .probe(&path)
                .expect("probe unsupported path")
                .detected
        );
    }

    let bad_magic = write_fixture("j2k", b"not a codestream");
    assert!(
        !backend
            .probe(&bad_magic.path)
            .expect("probe bad marker")
            .detected
    );
    assert!(
        !backend
            .probe(&bad_magic.path.with_file_name("missing.j2k"))
            .expect("probe missing file")
            .detected
    );
}

#[test]
fn open_exposes_codestream_resolution_levels_and_decodes_the_codestream() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let reader = RawJp2kBackend
        .open(&fixture.path)
        .expect("open raw JP2K fixture");
    let dataset = reader.dataset();

    assert_eq!(dataset.scenes.len(), 1);
    assert_eq!(dataset.scenes[0].name.as_deref(), Some("synthetic.j2k"));
    let series = &dataset.scenes[0].series[0];
    assert_eq!(series.axes, AxesShape::default());
    assert_eq!(series.sample_type, SampleType::Uint8);
    assert_eq!(series.channels.len(), 3);
    assert_eq!(series.channels[0].name.as_deref(), Some("R"));
    assert_eq!(series.channels[1].color, Some([0, 255, 0]));
    assert_eq!(series.channels[2].name.as_deref(), Some("B"));
    // Two wavelet decompositions yield two reduced levels that round up.
    let levels = series
        .levels
        .iter()
        .map(|level| (level.dimensions, level.downsample))
        .collect::<Vec<_>>();
    assert_eq!(levels, [((16, 12), 1.0), ((8, 6), 2.0), ((4, 3), 4.0)]);
    for level in &series.levels {
        assert!(matches!(
            level.tile_layout,
            TileLayout::Regular {
                tile_width,
                tile_height,
                tiles_across: 1,
                tiles_down: 1,
            } if (u64::from(tile_width), u64::from(tile_height)) == level.dimensions
        ));
    }
    assert!(dataset.associated_images.is_empty());
    assert!(dataset.icc_profiles.is_empty());
    assert_eq!(dataset.properties.vendor(), Some("raw-jp2k"));
    let quickhash = dataset
        .properties
        .quickhash1()
        .expect("raw JP2K quickhash property");
    assert_eq!(format!("{:032x}", dataset.id.get()), quickhash[..32]);

    let request = TileRequest::new(0, 0, 0, 0, 0);
    assert_eq!(reader.tile_codec_kind(&request), TileCodecKind::Jp2k);
    let tile = reader
        .read_tile_cpu(&request)
        .expect("decode raw JP2K tile");
    assert_eq!((tile.width(), tile.height()), (16, 12));
    assert_eq!(tile.channels(), 3);
    assert_eq!(tile.data.sample_type(), SampleType::Uint8);
    assert_eq!(tile.as_u8().expect("u8 JP2K pixels").len(), 16 * 12 * 3);

    let batch = reader
        .read_tiles_cpu(&[request.clone(), request.clone()])
        .expect("decode ordered raw JP2K batch");
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].as_u8(), batch[1].as_u8());
}

#[test]
fn raw_tile_preserves_the_original_codestream_and_metadata() {
    let fixture = write_fixture("j2c", RGB_CODESTREAM);
    let reader = RawJp2kBackend
        .open(&fixture.path)
        .expect("open raw JP2K fixture");
    let request = TileRequest::new(0, 0, 0, 0, 0);
    let raw = reader
        .read_raw_compressed_tile(&request)
        .expect("read raw compressed JP2K tile");

    assert_eq!(raw.compression(), Compression::Jp2kRgb);
    assert_eq!((raw.width(), raw.height()), (16, 12));
    assert_eq!(raw.bits_allocated(), 8);
    assert_eq!(raw.samples_per_pixel(), 3);
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Rgb
    );
    assert_eq!(raw.data(), RGB_CODESTREAM);

    let other_path = write_fixture("j2k", RGB_CODESTREAM);
    let other = RawJp2kBackend
        .open(&other_path.path)
        .expect("open same bytes at another path");
    assert_ne!(reader.dataset().id, other.dataset().id);
}

fn load_ppm(bytes: &[u8]) -> image::RgbImage {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Pnm)
        .expect("decode PPM reference")
        .to_rgb8()
}

/// A named codestream with its OpenJPEG reduction 1 and 2 references.
type ReducedReferenceCase = (&'static str, &'static [u8], [&'static [u8]; 2]);

/// References are OpenJPEG `-r 1`/`-r 2` decodes of the same codestreams,
/// covering irreversible 9/7 with and without MCT, reversible RCT, and a
/// multi-tile HTJ2K codestream with odd dimensions.
fn openjpeg_reduced_cases() -> [ReducedReferenceCase; 4] {
    [
        (
            "rgb_nomct",
            RGB_CODESTREAM,
            [
                include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.r1.ppm"),
                include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.r2.ppm"),
            ],
        ),
        (
            "rgb_mct",
            include_bytes!("../../../tests/fixtures/jp2k/rgb_mct.j2k"),
            [
                include_bytes!("../../../tests/fixtures/jp2k/rgb_mct.r1.ppm"),
                include_bytes!("../../../tests/fixtures/jp2k/rgb_mct.r2.ppm"),
            ],
        ),
        (
            "rgb_rct",
            include_bytes!("../../../tests/fixtures/jp2k/rgb_rct.j2k"),
            [
                include_bytes!("../../../tests/fixtures/jp2k/rgb_rct.r1.ppm"),
                include_bytes!("../../../tests/fixtures/jp2k/rgb_rct.r2.ppm"),
            ],
        ),
        (
            "openjph_rgb_u8_53",
            include_bytes!("../../../tests/fixtures/jp2k/openjph_rgb_u8_53.j2k"),
            [
                include_bytes!("../../../tests/fixtures/jp2k/openjph_rgb_u8_53.r1.ppm"),
                include_bytes!("../../../tests/fixtures/jp2k/openjph_rgb_u8_53.r2.ppm"),
            ],
        ),
    ]
}

#[test]
fn reduced_levels_match_openjpeg_reduced_decodes_exactly() {
    for (name, codestream, references) in openjpeg_reduced_cases() {
        let fixture = write_fixture("j2k", codestream);
        let reader = RawJp2kBackend.open(&fixture.path).expect(name);
        let levels = &reader.dataset().scenes[0].series[0].levels;
        assert_eq!(levels.len(), 3, "{name}");
        for (level, reference) in (1u32..).zip(references) {
            let expected = load_ppm(reference);
            let dimensions = (u64::from(expected.width()), u64::from(expected.height()));
            assert_eq!(
                levels[level as usize].dimensions, dimensions,
                "{name} {level}"
            );
            let tile = reader
                .read_tile_cpu(&TileRequest::new(0, 0, level, 0, 0))
                .expect(name);
            assert_eq!(
                (tile.width(), tile.height()),
                expected.dimensions(),
                "{name} level {level}"
            );
            assert_eq!(
                tile.as_u8(),
                Some(expected.as_raw().as_slice()),
                "{name} level {level}"
            );
        }
    }
}

/// Small enough to tile the 16x12 fixtures: a level wider or taller than 4 px
/// becomes 4 px tiles.
const SMALL_GRID: LevelGrid = LevelGrid {
    whole_level_max_side: 4,
    tile_side: 4,
};

fn open_with_grid(path: &std::path::Path, grid: LevelGrid) -> RawJp2kReader {
    open_raw_jp2k(path, BackendOpenConfig::deterministic(), grid).expect("open raw JP2K fixture")
}

/// Reads every tile of `level` as one batch and singly, checks each tile's
/// extent, and stitches the batch into the level's RGB8 pixels.
fn stitched_level(reader: &RawJp2kReader, level: u32) -> Vec<u8> {
    let meta = &reader.levels()[level as usize];
    let (width, height) = (meta.dimensions.0 as usize, meta.dimensions.1 as usize);
    let TileLayout::Regular {
        tile_width,
        tile_height,
        tiles_across,
        tiles_down,
    } = meta.tile_layout
    else {
        panic!("raw JP2K levels are regular grids");
    };
    let (tile_width, tile_height) = (tile_width as usize, tile_height as usize);
    let requests = (0..tiles_down)
        .flat_map(|row| (0..tiles_across).map(move |col| (col, row)))
        .map(|(col, row)| TileRequest::new(0, 0, level, col as i64, row as i64))
        .collect::<Vec<_>>();
    let batch = reader.read_tiles_cpu(&requests).expect("read tile batch");
    let mut pixels = vec![0; width * height * 3];
    for (request, tile) in requests.iter().zip(&batch) {
        let single = reader.read_tile_cpu(request).expect("read one tile");
        assert_eq!(single.as_u8(), tile.as_u8(), "level {level} {request:?}");
        let x0 = request.col as usize * tile_width;
        let y0 = request.row as usize * tile_height;
        let extent = ((width - x0).min(tile_width), (height - y0).min(tile_height));
        assert_eq!(
            (tile.width() as usize, tile.height() as usize),
            extent,
            "level {level} {request:?}"
        );
        let row_bytes = extent.0 * 3;
        let rows = tile.as_u8().expect("RGB8 tile").chunks_exact(row_bytes);
        for (y, row) in rows.enumerate() {
            let start = ((y0 + y) * width + x0) * 3;
            pixels[start..start + row_bytes].copy_from_slice(row);
        }
    }
    pixels
}

#[test]
fn tiled_levels_match_whole_level_decodes_exactly() {
    for (name, codestream, references) in openjpeg_reduced_cases() {
        let fixture = write_fixture("j2k", codestream);
        let tiled = open_with_grid(&fixture.path, SMALL_GRID);
        for (level, meta) in tiled.levels().iter().enumerate() {
            let (width, height) = meta.dimensions;
            let grid = if width.max(height) <= 4 {
                (1, 1)
            } else {
                (width.div_ceil(4), height.div_ceil(4))
            };
            assert!(
                matches!(
                    meta.tile_layout,
                    TileLayout::Regular { tiles_across, tiles_down, .. }
                        if (tiles_across, tiles_down) == grid
                ),
                "{name} level {level}"
            );
        }
        assert!(!is_one_tile(&tiled.levels()[0]), "{name}");

        let whole = RawJp2kBackend.open(&fixture.path).expect(name);
        let full = whole
            .read_tile_cpu(&TileRequest::new(0, 0, 0, 0, 0))
            .expect(name);
        assert_eq!(
            Some(stitched_level(&tiled, 0).as_slice()),
            full.as_u8(),
            "{name} level 0"
        );
        for (level, reference) in (1u32..).zip(references) {
            assert_eq!(
                stitched_level(&tiled, level),
                load_ppm(reference).into_raw(),
                "{name} level {level}"
            );
        }
    }
}

#[test]
fn default_grid_tiles_levels_over_4096_px_in_1024_px_tiles() {
    let (width, height) = (4100_u32, 4_u32);
    let pixels = (0..width * height * 3)
        .map(|index| (index * 31 % 251) as u8)
        .collect::<Vec<_>>();
    let options = j2k_native::EncodeOptions {
        reversible: true,
        num_decomposition_levels: 1,
        ..j2k_native::EncodeOptions::default()
    };
    let codestream =
        j2k_native::encode(&pixels, width, height, 3, 8, false, &options).expect("encode fixture");
    let fixture = write_fixture("j2c", &codestream);
    let reader = open_with_grid(&fixture.path, LevelGrid::DEFAULT);
    let layouts = reader
        .levels()
        .iter()
        .map(|level| match level.tile_layout {
            TileLayout::Regular {
                tile_width,
                tile_height,
                tiles_across,
                tiles_down,
            } => (tile_width, tile_height, tiles_across, tiles_down),
            _ => panic!("raw JP2K levels are regular grids"),
        })
        .collect::<Vec<_>>();
    assert_eq!(layouts, [(1024, 1024, 5, 1), (2050, 2, 1, 1)]);
    // Reversible coding is lossless, so the tiles reassemble the source.
    assert_eq!(stitched_level(&reader, 0), pixels);
}

#[test]
fn tiled_full_resolution_has_no_passthrough_or_adaptive_route() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let reader = open_with_grid(&fixture.path, SMALL_GRID);
    let first = TileRequest::new(0, 0, 0, 0, 0);
    assert!(matches!(
        reader.read_raw_compressed_tile(&first),
        Err(WsiError::Unsupported { .. })
    ));
    #[cfg(any(feature = "metal", feature = "cuda"))]
    assert!(reader
        .prepare_adaptive_jp2k(std::slice::from_ref(&first), 2, None)
        .is_none());
    // 16x12 in 4 px tiles has columns 0 to 3 and rows 0 to 2.
    for outside in [
        TileRequest::new(0, 0, 0, 4, 0),
        TileRequest::new(0, 0, 0, 0, 3),
    ] {
        assert!(matches!(
            reader.read_tile_cpu(&outside),
            Err(WsiError::TileRead { .. })
        ));
        assert!(matches!(
            reader.read_raw_compressed_tile(&outside),
            Err(WsiError::TileRead { .. })
        ));
    }
}

#[test]
fn exposed_levels_follow_the_codestream_decomposition_count() {
    let pixels = (0..8 * 8 * 3)
        .map(|index| (index * 7) as u8)
        .collect::<Vec<_>>();
    for (decompositions, expected) in [(0, vec![(8, 8)]), (3, vec![(8, 8), (4, 4), (2, 2), (1, 1)])]
    {
        let options = j2k_native::EncodeOptions {
            reversible: true,
            num_decomposition_levels: decompositions,
            ..j2k_native::EncodeOptions::default()
        };
        let codestream =
            j2k_native::encode(&pixels, 8, 8, 3, 8, false, &options).expect("encode fixture");
        let fixture = write_fixture("j2c", &codestream);
        let reader = RawJp2kBackend.open(&fixture.path).unwrap();
        let dimensions = reader.dataset().scenes[0].series[0]
            .levels
            .iter()
            .map(|level| level.dimensions)
            .collect::<Vec<_>>();
        assert_eq!(dimensions, expected, "{decompositions} decompositions");
        let smallest = u32::try_from(expected.len() - 1).unwrap();
        let tile = reader
            .read_tile_cpu(&TileRequest::new(0, 0, smallest, 0, 0))
            .unwrap();
        let (width, height) = expected[expected.len() - 1];
        assert_eq!(
            (u64::from(tile.width()), u64::from(tile.height())),
            (width, height)
        );
    }
}

#[test]
fn reduced_levels_have_no_passthrough_or_adaptive_route() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let managed = RawJp2kBackend
        .open_with_config(&fixture.path, BackendOpenConfig::deterministic())
        .unwrap();
    let reduced = TileRequest::new(0, 0, 1, 0, 0);
    assert_eq!(managed.tile_codec_kind(&reduced), TileCodecKind::Jp2k);
    assert!(matches!(
        managed.read_raw_compressed_tile(&reduced),
        Err(WsiError::Unsupported { .. })
    ));
    #[cfg(any(feature = "metal", feature = "cuda"))]
    assert!(managed
        .prepare_adaptive_jp2k(std::slice::from_ref(&reduced), 2, None)
        .is_none());
    let slide = crate::Slide::open(&fixture.path).unwrap();
    let tile = slide.read_tile(&reduced).unwrap();
    assert_eq!((tile.width(), tile.height()), (8, 6));
}

/// Metal decodes a batch of tiles, each tile alone, and a whole level twice,
/// covering the batch and single-image routes for full, region, reduced and
/// reduced-region requests.
#[cfg(feature = "metal")]
fn assert_metal_matches_cpu(
    reader: &RawJp2kReader,
    sessions: &crate::output::metal::MetalBackendSessions,
    name: &str,
) {
    for (level, meta) in reader.levels().iter().enumerate().take(4) {
        let level = level as u32;
        let TileLayout::Regular {
            tiles_across,
            tiles_down,
            ..
        } = meta.tile_layout
        else {
            panic!("raw JP2K levels are regular grids");
        };
        let mut batch = (0..tiles_down)
            .flat_map(|row| (0..tiles_across).map(move |col| (col, row)))
            .map(|(col, row)| TileRequest::new(0, 0, level, col as i64, row as i64))
            .collect::<Vec<_>>();
        if batch.len() == 1 {
            batch.push(batch[0].clone());
        }
        let cpu = reader.read_tiles_cpu(&batch).expect(name);
        let metal = reader.read_tiles_metal(&batch, sessions).expect(name);
        for ((request, expected), actual) in batch.iter().zip(&cpu).zip(&metal) {
            let context = format!("{name} level {level} {request:?}");
            let downloaded = actual.download_cpu().expect(&context);
            assert_eq!(downloaded.as_u8(), expected.as_u8(), "batch {context}");
            let single = reader
                .read_tiles_metal(std::slice::from_ref(request), sessions)
                .expect(&context);
            let downloaded = single[0].download_cpu().expect(&context);
            assert_eq!(downloaded.as_u8(), expected.as_u8(), "single {context}");
        }
    }
}

#[cfg(feature = "metal")]
#[test]
fn metal_tiles_match_cpu_tiles_at_every_device_level() {
    let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() else {
        return;
    };
    for (name, codestream, _) in openjpeg_reduced_cases() {
        let fixture = write_fixture("j2k", codestream);
        for (grid_name, grid) in [("small", SMALL_GRID), ("default", LevelGrid::DEFAULT)] {
            // J2K's Metal batch rejects these tiles; see the ignored test below.
            if name == "openjph_rgb_u8_53" && grid_name == "small" {
                continue;
            }
            let reader = open_with_grid(&fixture.path, grid);
            assert_metal_matches_cpu(&reader, &sessions, name);
        }
    }
}

/// J2K's Metal batch prepare fails on half-scale regions in the top row of
/// this multi-tile HTJ2K codestream ("failed to decode code-block") while the
/// same region decodes alone. Read as a batch, such tiles return an error and
/// callers fall back to the CPU; no wrong pixels result. Remove the ignore when
/// J2K decodes them.
#[cfg(feature = "metal")]
#[test]
#[ignore = "J2K Metal batch prepare rejects top-row half-scale regions of multi-tile HTJ2K"]
fn metal_batches_half_scale_regions_of_multi_tile_htj2k() {
    let sessions = crate::output::metal::MetalBackendSessions::system_default()
        .expect("Metal device for the J2K batch regression");
    let (name, codestream, _) = openjpeg_reduced_cases()[3];
    assert_eq!(name, "openjph_rgb_u8_53");
    let fixture = write_fixture("j2k", codestream);
    let reader = open_with_grid(&fixture.path, SMALL_GRID);
    // Source region (0, 0, 8, 8) at half scale, twice.
    let tile = TileRequest::new(0, 0, 1, 0, 0);
    let batch = [tile.clone(), tile.clone()];
    let metal = reader
        .read_tiles_metal(&batch, &sessions)
        .expect("Metal batch of the top-left half-scale region");
    let cpu = reader.read_tile_cpu(&tile).unwrap();
    for actual in &metal {
        assert_eq!(actual.download_cpu().unwrap().as_u8(), cpu.as_u8());
    }
}

#[cfg(feature = "metal")]
#[test]
fn metal_leaves_reductions_past_three_levels_to_the_cpu() {
    let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() else {
        return;
    };
    let pixels = (0..32 * 32 * 3)
        .map(|index| (index * 7) as u8)
        .collect::<Vec<_>>();
    let options = j2k_native::EncodeOptions {
        reversible: true,
        num_decomposition_levels: 4,
        ..j2k_native::EncodeOptions::default()
    };
    let codestream =
        j2k_native::encode(&pixels, 32, 32, 3, 8, false, &options).expect("encode fixture");
    let fixture = write_fixture("j2c", &codestream);
    let reader = open_with_grid(&fixture.path, LevelGrid::DEFAULT);
    let deepest = TileRequest::new(0, 0, 4, 0, 0);
    assert!(matches!(
        reader.read_tiles_metal(std::slice::from_ref(&deepest), &sessions),
        Err(WsiError::Unsupported { .. })
    ));
    let tile = reader
        .read_tile_cpu(&deepest)
        .expect("CPU decode of level 4");
    assert_eq!((tile.width(), tile.height()), (2, 2));
}

#[test]
fn request_validation_rejects_non_default_axes_and_tiles() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let reader = RawJp2kBackend
        .open(&fixture.path)
        .expect("open raw JP2K fixture");

    for request in [
        TileRequest::new(1, 0, 0, 0, 0),
        TileRequest::new(0, 1, 0, 0, 0),
        TileRequest::new(0, 0, 3, 0, 0),
        TileRequest::new(0, 0, 0, -1, 0),
        TileRequest::new(0, 0, 0, 0, 1),
    ] {
        assert!(matches!(
            reader.read_tile_cpu(&request),
            Err(WsiError::TileRead { .. })
        ));
        assert!(matches!(
            reader.read_raw_compressed_tile(&request),
            Err(WsiError::TileRead { .. })
        ));
    }

    let non_default_plane =
        TileRequest::new(0, 0, 0, 0, 0).with_plane(PlaneSelection { z: 1, c: 0, t: 0 });
    assert!(matches!(
        reader.read_tile_cpu(&non_default_plane),
        Err(WsiError::Unsupported { .. })
    ));
}

#[test]
fn open_reports_path_context_and_rejects_truncated_or_unsupported_codestreams() {
    let missing_dir = tempfile::tempdir().expect("temporary missing-file directory");
    let missing_path = missing_dir.path().join("missing.j2k");
    let error = expect_error(
        RawJp2kBackend.open(&missing_path),
        "opening a missing JP2K file must fail",
    );
    assert!(matches!(error, WsiError::IoWithPath { path, .. } if path == missing_path));

    for bytes in [&b"\xff\x4f"[..], &b"\xff\x4f\xff\x51\0\x02"[..]] {
        let fixture = write_fixture("j2k", bytes);
        assert!(RawJp2kBackend.open(&fixture.path).is_err());
    }
}

#[test]
fn configured_encoded_limit_rejects_before_reading_the_codestream() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let limits = crate::SlideLimits::default()
        .with_encoded_unit_bytes(1)
        .expect("nonzero encoded limit");
    let config = BackendOpenConfig::new(crate::CacheConfig::deterministic(), limits);
    let error = expect_error(
        RawJp2kBackend.open_with_config(&fixture.path, config),
        "tiny encoded limit must reject raw JP2K input",
    );
    assert!(matches!(error, WsiError::ResourceLimit { .. }));
}

#[test]
fn managed_reader_reports_encoded_working_set_bounds() {
    let fixture = write_fixture("j2k", RGB_CODESTREAM);
    let reader = RawJp2kBackend
        .open_with_config(&fixture.path, BackendOpenConfig::deterministic())
        .expect("open managed raw JP2K fixture");
    let encoded_len = RGB_CODESTREAM.len() as u64;
    let tile = TileRequest::new(0, 0, 0, 0, 0);
    let display = TileViewRequest::new(0, 0, 0, 0, 0, 16, 12);
    let region = RegionRequest::new(0, 0, 0, (0, 0), (16, 12));

    assert_eq!(reader.tile_encoded_upper_bound(&tile).unwrap(), encoded_len);
    assert_eq!(reader.tile_batch_encoded_upper_bound(&[]).unwrap(), 0);
    assert_eq!(
        reader
            .tile_batch_encoded_upper_bound(&[tile.clone(), tile])
            .unwrap(),
        encoded_len
    );
    assert_eq!(
        reader.display_tile_encoded_upper_bound(&display).unwrap(),
        encoded_len
    );
    assert_eq!(reader.associated_encoded_upper_bound("label").unwrap(), 0);
    assert_eq!(
        reader.region_fastpath_encoded_upper_bound(&region).unwrap(),
        encoded_len
    );
}

#[test]
fn open_rejects_truncated_packet_data_during_reduction_probing() {
    let data = include_bytes!("../../../tests/fixtures/jp2k/truncated_tall_tag_tree.j2k");
    let fixture = write_fixture("j2k", data);
    let error = expect_error(
        RawJp2kBackend.open(&fixture.path),
        "truncated packet data must not be exposed as a full-resolution-only slide",
    );
    assert!(matches!(error, WsiError::Jp2k(_)), "{error}");
}

#[test]
fn open_rejects_tile_grids_that_cannot_fit_in_the_codestream() {
    let mut data = RGB_CODESTREAM.to_vec();
    // Retain the original 16x12 tile and its only tile part, but claim a
    // 256x256 tile grid. Missing tile parts must not drive a full decode of
    // this tiny input.
    data[8..12].copy_from_slice(&4096_u32.to_be_bytes());
    data[12..16].copy_from_slice(&3072_u32.to_be_bytes());
    let fixture = write_fixture("j2k", &data);
    let error = expect_error(
        RawJp2kBackend.open(&fixture.path),
        "a codestream too short to contain its declared tile grid must fail",
    );
    assert!(matches!(error, WsiError::Jp2k(_)), "{error}");
}

#[test]
fn reduction_probing_preserves_shorter_component_ladders() {
    let mut data = RGB_CODESTREAM.to_vec();
    let cod = data
        .windows(2)
        .position(|marker| marker == [0xff, 0x52])
        .unwrap();
    let end = cod + 2 + usize::from(u16::from_be_bytes([data[cod + 2], data[cod + 3]]));
    let parameters = data[cod + 9..end].to_vec();
    assert_eq!(parameters.len(), 5);
    // Advertise an extra global level, retaining the encoded two-level
    // decomposition for each component through COC overrides.
    data[cod + 9] += 1;
    let mut overrides = Vec::new();
    for component in 0..3 {
        overrides.extend_from_slice(&[0xff, 0x53, 0, 9, component, 0]);
        overrides.extend_from_slice(&parameters);
    }
    data.splice(end..end, overrides);
    assert_eq!(jp2k_decodable_reduction_levels(&data).unwrap(), 2);
}
