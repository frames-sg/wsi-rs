use std::fs;
use std::path::Path;

use super::scene::header::{
    fourcc_matches, sample_type_from_ets, validate_ets_chunk_table, validate_ets_header_limits,
    MAX_ETS_DIMENSIONS, MAX_ETS_TILES,
};
use super::scene::index::{
    checked_ets_axis_len, checked_ets_extent, checked_ets_level_count, key_from_coords,
    MAX_ETS_AXIS_INDEX, MAX_ETS_LEVEL_INDEX,
};
use super::scene::EtsScene;
use super::*;
use crate::core::registry::Slide;
use crate::core::types::*;

mod fixtures;

use fixtures::*;

fn invalid_slide_message(error: WsiError) -> String {
    match error {
        WsiError::InvalidSlide { message, .. } => message,
        other => panic!("expected InvalidSlide, got {other:?}"),
    }
}

fn expect_wsi_error<T>(result: Result<T, WsiError>, context: &str) -> WsiError {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

#[test]
fn configured_tile_index_limit_is_enforced_across_ets_scenes() {
    let fixture = write_vsi_fixture(&[
        ("first", EtsSpec::default()),
        ("second", EtsSpec::default()),
    ]);
    let limits = crate::SlideLimits::default()
        .with_tile_index_bytes(1)
        .expect("nonzero tile-index limit");
    let config = BackendOpenConfig::new(crate::CacheConfig::deterministic(), limits);

    let error = expect_wsi_error(
        OlympusVsiBackend.open_with_config(&fixture.path, config),
        "tiny configured tile-index limit must reject ETS before allocation",
    );
    assert!(matches!(
        error,
        WsiError::ResourceLimit {
            resource: "tile/frame index",
            ..
        }
    ));
}

#[test]
fn probe_requires_case_insensitive_vsi_extension_and_companion_directory() {
    let fixture = write_vsi_fixture(&[("scene", EtsSpec::default())]);
    let backend = OlympusVsiBackend;

    let result = backend.probe(&fixture.path).expect("probe synthetic VSI");
    assert!(result.detected);
    assert_eq!(result.vendor, "olympus");
    assert_eq!(result.confidence, ProbeConfidence::Definite);

    let uppercase = fixture.path.with_extension("VSI");
    fs::rename(&fixture.path, &uppercase).expect("rename VSI fixture");
    let uppercase_result = backend.probe(&uppercase).expect("probe uppercase VSI");
    assert!(uppercase_result.detected);

    let wrong_extension = uppercase.with_extension("tif");
    let wrong_result = backend
        .probe(&wrong_extension)
        .expect("probe non-VSI extension");
    assert!(!wrong_result.detected);
    assert!(wrong_result.vendor.is_empty());
    assert_eq!(wrong_result.confidence, ProbeConfidence::Definite);

    fs::remove_dir_all(companion_dir(&uppercase).expect("companion path"))
        .expect("remove companion directory");
    assert!(
        !backend
            .probe(&uppercase)
            .expect("probe orphan VSI")
            .detected
    );
}

#[cfg(unix)]
#[test]
fn ets_discovery_does_not_follow_symlinked_scene_directories() {
    use super::slide::find_ets_files;
    use crate::core::registry::OpenBudget;
    use std::os::unix::fs::symlink;

    let companion = tempfile::tempdir().expect("companion directory");
    let outside = tempfile::tempdir().expect("outside directory");
    let outside_scene = outside.path().join("scene");
    fs::create_dir(&outside_scene).expect("outside ETS scene");
    fs::write(outside_scene.join("frame_t.ets"), b"ETS\0").expect("outside ETS file");
    symlink(&outside_scene, companion.path().join("linked-scene"))
        .expect("symlink outside scene into companion");

    let budget = OpenBudget::new(crate::SlideLimits::default());
    assert!(
        find_ets_files(companion.path(), &budget)
            .expect("scan companion directory")
            .is_empty(),
        "ETS discovery must not escape through a directory symlink"
    );
}

#[test]
fn synthetic_vsi_opens_with_metadata_and_reads_encoded_and_background_tiles() {
    let fixture = write_vsi_fixture(&[("large-scene", EtsSpec::default())]);
    let backend = OlympusVsiBackend;
    let reader = backend.open(&fixture.path).expect("open synthetic VSI");
    let dataset = reader.dataset();

    assert_eq!(dataset.properties.vendor(), Some("olympus"));
    let quickhash = dataset
        .properties
        .quickhash1()
        .expect("Olympus quickhash property");
    assert_eq!(format!("{:032x}", dataset.id.get()), quickhash[..32]);
    assert_eq!(dataset.scenes.len(), 1);
    assert_eq!(dataset.scenes[0].name.as_deref(), Some("large-scene"));
    let series = &dataset.scenes[0].series[0];
    assert_eq!(series.axes, AxesShape::default());
    assert_eq!(series.sample_type, SampleType::Uint8);
    assert!(series.channels.is_empty());
    assert_eq!(series.levels.len(), 1);
    assert_eq!(series.levels[0].dimensions, (32, 12));
    assert_eq!(series.levels[0].downsample, 1.0);
    assert!(matches!(
        series.levels[0].tile_layout,
        TileLayout::Regular {
            tile_width: 16,
            tile_height: 12,
            tiles_across: 2,
            tiles_down: 1,
        }
    ));

    let background_req = TileRequest::new(0, 0, 0, 0, 0);
    let background = reader
        .read_tile_cpu(&background_req)
        .expect("read absent tile as ETS background");
    assert_eq!((background.width(), background.height()), (16, 12));
    assert_eq!(background.channels(), 3);
    assert_eq!(background.color_space(), &ColorSpace::Rgb);
    assert_eq!(background.layout(), CpuTileLayout::Interleaved);
    assert!(background
        .as_u8()
        .expect("U8 background")
        .chunks_exact(3)
        .all(|pixel| pixel == [7, 11, 13]));

    let encoded_req = TileRequest::new(0, 0, 0, 1, 0);
    let encoded = reader
        .read_tile_cpu(&encoded_req)
        .expect("decode embedded JP2K tile");
    assert_eq!((encoded.width(), encoded.height()), (16, 12));
    assert_eq!(encoded.channels(), 3);
    assert_eq!(encoded.as_u8().expect("decoded U8 tile").len(), 16 * 12 * 3);
    assert_ne!(encoded.as_u8(), background.as_u8());

    let batch = reader
        .read_tiles_cpu(&[background_req, encoded_req])
        .expect("read ordered Olympus batch");
    assert_eq!(batch.len(), 2);
    assert_eq!(
        batch.into_iter().next().unwrap().as_u8(),
        background.as_u8()
    );
}

#[test]
fn open_vsi_keeps_payloads_bound_to_its_parsed_ets_file() {
    let fixture = write_vsi_fixture(&[("scene", EtsSpec::default())]);
    let reader = OlympusVsiBackend.open(&fixture.path).unwrap();
    let req = TileRequest::new(0, 0, 0, 1, 0);
    let expected = reader.read_tile_cpu(&req).unwrap();
    let ets = companion_dir(&fixture.path)
        .unwrap()
        .join("scene/frame_t.ets");
    fs::rename(&ets, ets.with_extension("retained")).unwrap();
    fs::write(&ets, b"replacement is not the parsed ETS source").unwrap();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| reader.read_tiles_cpu(&[req.clone(), req.clone()]).unwrap()))
            .collect();
        for handle in handles {
            for actual in handle.join().unwrap() {
                assert_eq!(actual.as_u8(), expected.as_u8());
            }
        }
    });
}

#[test]
fn vsi_submits_one_codec_batch_and_preserves_sparse_duplicate_slots() {
    use crate::core::execution_telemetry::{test_count, Event};
    let fixture = write_vsi_fixture(&[("scene", EtsSpec::default())]);
    let reader = OlympusVsiBackend.open(&fixture.path).unwrap();
    let reqs = [
        TileRequest::new(0, 0, 0, 1, 0),
        TileRequest::new(0, 0, 0, 0, 0),
        TileRequest::new(0, 0, 0, 1, 0),
    ];
    let expected: Vec<_> = reqs
        .iter()
        .map(|req| reader.read_tile_cpu(req).unwrap())
        .collect();
    let before = test_count(Event::CpuJp2kBatches);
    let actual = reader.read_tiles_cpu(&reqs).unwrap();
    assert_eq!(test_count(Event::CpuJp2kBatches) - before, 1);
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual.iter().zip(&expected) {
        assert_eq!(a.as_u8(), b.as_u8());
    }
}

#[test]
fn admitted_vsi_batch_uses_payload_sizes_and_keeps_sparse_duplicate_pixels() {
    let fixture = write_vsi_fixture(&[("scene", EtsSpec::default())]);
    let limits = crate::SlideLimits::default()
        .with_operation_transient_bytes(8 * 1024)
        .unwrap();
    let slide = Slide::open_with_options(
        &fixture.path,
        crate::SlideOpenOptions::default()
            .with_limits(limits)
            .with_cache_config(crate::CacheConfig::deterministic()),
    )
    .unwrap();
    let reqs = [
        TileRequest::new(0, 0, 0, 1, 0),
        TileRequest::new(0, 0, 0, 0, 0),
        TileRequest::new(0, 0, 0, 1, 0),
    ];
    let expected: Vec<_> = reqs
        .iter()
        .map(|req| slide.read_tile(req).unwrap())
        .collect();
    let actual = slide.read_tiles(&reqs).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.as_u8(), expected.as_u8());
    }
}

#[test]
fn vsi_batch_decode_and_retained_file_errors_identify_the_failed_tile() {
    let spec = EtsSpec {
        chunks: vec![ChunkSpec::new(&[1, 0, 0], b"invalid codestream")],
        ..EtsSpec::default()
    };
    let fixture = write_vsi_fixture(&[("scene", spec)]);
    let reader = OlympusVsiBackend.open(&fixture.path).unwrap();
    let present = TileRequest::new(0, 0, 0, 1, 0);
    let sparse = TileRequest::new(0, 0, 0, 0, 0);
    for error in [
        reader.read_tile_cpu(&present).unwrap_err(),
        reader
            .read_tiles_cpu(&[sparse.clone(), present.clone()])
            .unwrap_err(),
    ] {
        assert!(
            matches!(error, WsiError::TileRead { col: 1, row: 0, level: 0, reason } if !reason.is_empty())
        );
    }
    let ets = companion_dir(&fixture.path)
        .unwrap()
        .join("scene/frame_t.ets");
    fs::OpenOptions::new()
        .write(true)
        .open(&ets)
        .unwrap()
        .set_len(0)
        .unwrap();
    let error = reader.read_tiles_cpu(&[sparse, present]).unwrap_err();
    assert!(
        matches!(error, WsiError::TileRead { col: 1, row: 0, level: 0, reason } if reason.contains("frame_t.ets"))
    );
}

#[test]
fn reader_reports_scene_series_level_plane_and_tile_bounds() {
    let fixture = write_vsi_fixture(&[("scene", EtsSpec::default())]);
    let reader = OlympusVsiBackend
        .open(&fixture.path)
        .expect("open synthetic VSI");

    let error = reader
        .read_tile_cpu(&TileRequest::new(1, 0, 0, 0, 0))
        .expect_err("scene must be checked");
    assert!(matches!(
        error,
        WsiError::SceneOutOfRange { index: 1, count: 1 }
    ));

    let error = reader
        .read_tile_cpu(&TileRequest::new(0, 1, 0, 0, 0))
        .expect_err("series must be checked");
    assert!(matches!(
        error,
        WsiError::SeriesOutOfRange { index: 1, count: 1 }
    ));

    let error = reader
        .read_tile_cpu(&TileRequest::new(0, 0, 1, 0, 0))
        .expect_err("level must be checked");
    assert!(matches!(
        error,
        WsiError::LevelOutOfRange { level: 1, count: 1 }
    ));

    let plane_req = TileRequest::new(0, 0, 0, 0, 0).with_plane(PlaneSelection { z: 1, c: 0, t: 0 });
    let error = reader
        .read_tile_cpu(&plane_req)
        .expect_err("plane must be checked");
    assert!(matches!(error, WsiError::PlaneOutOfRange { axis, value: 1, max: 0 } if axis == "z"));

    for request in [
        TileRequest::new(0, 0, 0, -1, 0),
        TileRequest::new(0, 0, 0, 2, 0),
        TileRequest::new(0, 0, 0, 0, -1),
        TileRequest::new(0, 0, 0, 0, 1),
    ] {
        assert!(matches!(
            reader.read_tile_cpu(&request),
            Err(WsiError::TileRead { .. })
        ));
    }
}

#[test]
fn parser_builds_pyramids_channels_and_orders_scenes_by_area() {
    let mut multichannel = EtsSpec {
        n_dimensions: 4,
        samples_per_pixel: 1,
        background: vec![29],
        chunks: vec![ChunkSpec::new(&[1, 0, 0, 1], RGB_CODESTREAM)],
        ..EtsSpec::default()
    };
    multichannel.tile_width = 8;
    multichannel.tile_height = 6;

    let pyramid = EtsSpec {
        n_dimensions: 4,
        use_pyramid: true,
        chunks: vec![
            ChunkSpec::new(&[1, 0, 0, 0], RGB_CODESTREAM),
            ChunkSpec::new(&[0, 0, 0, 1], RGB_CODESTREAM),
        ],
        ..EtsSpec::default()
    };
    let fixture = write_vsi_fixture(&[("smaller", multichannel), ("pyramid", pyramid)]);
    let slide = OlympusVsiSlide::parse(&fixture.path).expect("parse multi-scene VSI");

    assert_eq!(slide.dataset.scenes[0].name.as_deref(), Some("pyramid"));
    assert_eq!(slide.dataset.scenes[0].series[0].levels.len(), 2);
    let channels = &slide.dataset.scenes[1].series[0].channels;
    assert_eq!(slide.dataset.scenes[1].series[0].axes.c, 2);
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0].name.as_deref(), Some("Channel 0"));
    assert_eq!(channels[1].name.as_deref(), Some("Channel 1"));

    let background = slide.scenes[1]
        .background_tile(2, 2)
        .expect("grayscale ETS background");
    assert_eq!(
        background.as_u8(),
        Some([29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29, 29].as_slice())
    );
}

#[test]
fn open_reports_missing_companion_and_missing_ets_files() {
    let temp = tempfile::tempdir().expect("temporary VSI directory");
    let path = temp.path().join("orphan.vsi");
    drop(cfb::create(&path).expect("create minimal compound VSI file"));

    let error = expect_wsi_error(
        OlympusVsiBackend.open(&path),
        "missing companion directory must fail",
    );
    assert!(matches!(
        error,
        WsiError::IoWithPath { path, .. } if path == temp.path().join("_orphan_")
    ));

    fs::create_dir(temp.path().join("_orphan_")).expect("create empty companion directory");
    let error = expect_wsi_error(
        OlympusVsiBackend.open(&path),
        "companion without ETS must fail",
    );
    assert_eq!(invalid_slide_message(error), "no ETS frame files found");
}

#[test]
fn parser_rejects_invalid_headers_and_chunk_tables() {
    let cases: Vec<(&str, Vec<u8>, &str)> = {
        let valid = build_ets(&EtsSpec::default());
        let mut invalid_sis = valid.clone();
        invalid_sis[0..4].copy_from_slice(b"BAD\0");
        let mut invalid_ets = valid.clone();
        invalid_ets[ADDITIONAL_HEADER_OFFSET..ADDITIONAL_HEADER_OFFSET + 4]
            .copy_from_slice(b"BAD\0");
        let mut truncated_table = valid;
        truncated_table.truncate(CHUNK_TABLE_OFFSET + 31);
        vec![
            ("invalid SIS", invalid_sis, "invalid ETS SIS magic"),
            ("invalid ETS", invalid_ets, "invalid ETS header magic"),
            ("truncated table", truncated_table, "ETS chunk-table range"),
        ]
    };

    for (label, bytes, expected) in cases {
        let (_temp, path) = write_ets(&bytes);
        let message = invalid_slide_message(expect_wsi_error(EtsScene::parse(&path), label));
        assert!(message.contains(expected), "{label}: {message}");
    }
}

#[test]
fn parser_rejects_unsupported_compression_and_invalid_chunk_records() {
    let mut cases = Vec::new();

    let unsupported = EtsSpec {
        compression: 99,
        ..EtsSpec::default()
    };
    cases.push((unsupported, "unsupported ETS compression 99"));

    let mut empty = EtsSpec::default();
    empty.chunks[0].declared_len = Some(0);
    cases.push((empty, "ETS tile payload is empty"));

    let mut overflow = EtsSpec::default();
    overflow.chunks[0].declared_offset = Some(u64::MAX);
    cases.push((overflow, "ETS tile payload range overflows"));

    let mut out_of_bounds = EtsSpec::default();
    out_of_bounds.chunks[0].declared_offset = Some(10_000);
    cases.push((out_of_bounds, "exceeds file length"));

    let duplicate = EtsSpec {
        chunks: vec![
            ChunkSpec::new(&[0, 0, 0], RGB_CODESTREAM),
            ChunkSpec::new(&[0, 0, 0], RGB_CODESTREAM),
        ],
        ..EtsSpec::default()
    };
    cases.push((duplicate, "duplicate ETS tile coordinates"));

    let negative = EtsSpec {
        chunks: vec![ChunkSpec::new(&[-1, 0, 0], RGB_CODESTREAM)],
        ..EtsSpec::default()
    };
    cases.push((negative, "negative ETS x coordinate -1"));

    let excessive_level = EtsSpec {
        n_dimensions: 4,
        use_pyramid: true,
        chunks: vec![ChunkSpec::new(
            &[0, 0, 0, MAX_ETS_LEVEL_INDEX as i32 + 1],
            RGB_CODESTREAM,
        )],
        ..EtsSpec::default()
    };
    cases.push((excessive_level, "ETS level index"));

    let excessive_axis = EtsSpec {
        n_dimensions: 4,
        chunks: vec![ChunkSpec::new(
            &[0, 0, MAX_ETS_AXIS_INDEX as i32 + 1, 0],
            RGB_CODESTREAM,
        )],
        ..EtsSpec::default()
    };
    cases.push((excessive_axis, "ETS z index"));

    for (spec, expected) in cases {
        let (_temp, path) = write_ets(&build_ets(&spec));
        let message = invalid_slide_message(expect_wsi_error(EtsScene::parse(&path), expected));
        assert!(message.contains(expected), "unexpected error: {message}");
    }
}

#[test]
fn coordinate_and_type_parsers_preserve_ets_axis_semantics() {
    assert_eq!(
        key_from_coords(&[4, 5, 6, 7, 8], false).expect("non-pyramid coordinates"),
        EtsTileKey {
            level: 0,
            z: 6,
            c: 7,
            t: 8,
            col: 4,
            row: 5,
        }
    );
    assert_eq!(
        key_from_coords(&[4, 5, 6, 7, 8, 9], true).expect("pyramid coordinates"),
        EtsTileKey {
            level: 9,
            z: 6,
            c: 7,
            t: 8,
            col: 4,
            row: 5,
        }
    );
    assert!(key_from_coords(&[0, 1], false).is_err());
    assert!(key_from_coords(&[0, 1, -1], false).is_err());
    assert!(key_from_coords(&[0, 1, 2, -1], true).is_err());

    assert_eq!(sample_type_from_ets(1).unwrap(), SampleType::Uint8);
    assert_eq!(sample_type_from_ets(2).unwrap(), SampleType::Uint8);
    assert_eq!(sample_type_from_ets(3).unwrap(), SampleType::Uint16);
    assert_eq!(sample_type_from_ets(4).unwrap(), SampleType::Uint16);
    assert_eq!(sample_type_from_ets(9).unwrap(), SampleType::Float32);
    assert!(matches!(
        sample_type_from_ets(10),
        Err(WsiError::UnsupportedFormat(message)) if message.contains("pixel type 10")
    ));

    assert!(fourcc_matches(b"SIS\0", b"SIS"));
    assert!(fourcc_matches(b"SIS ", b"SIS"));
    assert!(!fourcc_matches(b"SIS!", b"SIS"));
}

#[test]
fn ets_limits_validate_header_table_axes_and_extents() {
    validate_ets_header_limits(3, 1, 1, 1, 1, 1).expect("minimal ETS header");
    validate_ets_header_limits(MAX_ETS_DIMENSIONS, MAX_ETS_TILES, 40, 1, u32::MAX, u32::MAX)
        .expect("declared ETS limits");

    assert!(validate_ets_header_limits(MAX_ETS_DIMENSIONS + 1, 1, 1, 1, 1, 1).is_err());
    assert!(validate_ets_header_limits(3, 0, 1, 1, 1, 1).is_err());
    assert!(validate_ets_header_limits(3, MAX_ETS_TILES + 1, 1, 1, 1, 1).is_err());
    assert!(validate_ets_header_limits(3, 1, 0, 1, 1, 1).is_err());
    assert!(validate_ets_header_limits(3, 1, 41, 1, 1, 1).is_err());
    assert!(validate_ets_header_limits(3, 1, 1, 1, 0, 1).is_err());

    validate_ets_chunk_table(64, 32, 3, 1).expect("exact chunk-table boundary");
    assert!(validate_ets_chunk_table(63, 32, 3, 1).is_err());
    assert!(validate_ets_chunk_table(u64::MAX, u64::MAX, 3, 1).is_err());

    assert_eq!(checked_ets_level_count(0).unwrap(), 1);
    assert_eq!(checked_ets_level_count(MAX_ETS_LEVEL_INDEX).unwrap(), 1024);
    assert!(checked_ets_level_count(MAX_ETS_LEVEL_INDEX + 1).is_err());
    assert_eq!(
        checked_ets_axis_len(MAX_ETS_AXIS_INDEX, "z").expect("maximum ETS axis"),
        MAX_ETS_AXIS_INDEX + 1
    );
    assert!(checked_ets_axis_len(MAX_ETS_AXIS_INDEX + 1, "z").is_err());
    assert_eq!(checked_ets_extent(15, 256, "width").unwrap(), 4096);
    assert!(checked_ets_extent(u32::MAX, 1, "width").is_err());
    assert!(checked_ets_extent(1, u32::MAX, "width").is_err());
}

#[test]
fn opens_olympus_vsi_when_corpus_is_available() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let path =
        workspace_root.join("downloads/openslide-testdata-extracted/full/Olympus/OS-1/OS-1.vsi");
    if !path.exists() {
        return;
    }

    let slide = Slide::open(&path).expect("open Olympus VSI");
    let dataset = slide.dataset();
    assert!(!dataset.scenes.is_empty());
    assert!(dataset.scenes[0].series[0].levels.len() >= 2);
    let tile = slide
        .read_tile(&TileRequest::new(0, 0, 0, 0, 0))
        .expect("read Olympus VSI tile");
    assert!(tile.width() > 0);
    assert!(tile.height() > 0);
}
