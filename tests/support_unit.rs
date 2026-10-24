mod support;

use std::path::PathBuf;

use wsi_rs::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout};

use support::compare::{compare_rgba, tolerance_failure, Tolerance};
use support::corpus::{parse_manifest, public_manifest_path};
use support::oracles::{
    is_reference_oracle_unsupported, read_probe, require_reference_tile, sample_buffer_to_rgba,
    top_left_probe, J2kOracle, OpenedSlide, Oracle, ProbeKind, ProbeRequest, ReferenceOracle,
    ReferenceTileError, TileBuffer,
};

#[test]
fn public_prelude_exports_common_reader_types() {
    use wsi_rs::prelude::{
        LevelIdx as PreludeLevelIdx, PlaneIdx as PreludePlaneIdx,
        PlaneSelection as PreludePlaneSelection, RegionRequest as PreludeRegionRequest,
        SceneId as PreludeSceneId, SeriesId as PreludeSeriesId, Slide as PreludeSlide,
        TileRequest as PreludeTileRequest, WsiError as PreludeWsiError,
    };

    let plane = PreludePlaneSelection::default();
    let region = PreludeRegionRequest::new(
        PreludeSceneId::new(0),
        PreludeSeriesId::new(0),
        PreludeLevelIdx::new(0),
        (0, 0),
        (16, 16),
    )
    .with_plane(PreludePlaneIdx::new(plane));
    let tile = PreludeTileRequest::new(0usize, 0usize, 0u32, 0, 0).with_plane(plane);

    assert_eq!(region.size_px, (16, 16));
    assert_eq!(tile.col, 0);
    let _ = std::any::type_name::<PreludeSlide>();
    let _ = std::any::type_name::<PreludeWsiError>();
}

#[test]
fn compare_identical_buffers_pass_with_psnr_inf() {
    let a = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
    let report = compare_rgba(&a, &a, Tolerance::JPEG_TIGHT);

    assert!(report.passed);
    assert_eq!(report.max_abs, 0);
    assert_eq!(report.mean_abs, 0.0);
    assert!(report.psnr_db.is_infinite());
    assert_eq!(report.bytewise_equal_rate, 1.0);
}

#[test]
fn compare_off_by_one_passes_jpeg_tight() {
    let a = vec![10u8; 32];
    let mut b = a.clone();
    b[0] = 11;

    let report = compare_rgba(&a, &b, Tolerance::JPEG_TIGHT);

    assert!(report.passed);
    assert_eq!(report.max_abs, 1);
}

#[test]
fn compare_off_by_two_fails_jpeg_tight_passes_tolerant() {
    let a = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
    let b = vec![12u8, 20, 30, 255, 40, 50, 60, 255];

    let tight = compare_rgba(&a, &b, Tolerance::JPEG_TIGHT);
    let tolerant = compare_rgba(&a, &b, Tolerance::TOLERANT);

    assert!(!tight.passed);
    assert!(tolerant.passed);
}

#[test]
fn jpeg_decoder_compat_accepts_cross_platform_huron_mean_variance() {
    let expected = [0, 0, 0, 255].repeat(8);
    let mut actual = expected.clone();
    actual[..3].fill(1);

    let report = compare_rgba(&actual, &expected, Tolerance::JPEG_DECODER_COMPAT);

    assert_eq!(report.max_abs, 1);
    assert_eq!(report.mean_abs, 0.125);
    assert!(report.passed);
}

#[test]
fn compare_requires_exact_alpha_and_excludes_it_from_rgb_mean() {
    let actual = vec![10u8, 20, 30, 0];
    let expected = vec![10u8, 20, 30, 255];

    let report = compare_rgba(&actual, &expected, Tolerance::TOLERANT);

    assert!(!report.passed);
    assert!(!report.alpha_exact);
    assert_eq!(report.max_abs, 0);
    assert_eq!(report.mean_abs, 0.0);
}

#[test]
fn compare_tolerance_failure_formats_failed_report() {
    let a = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
    let b = vec![12u8, 20, 30, 255, 40, 50, 60, 255];
    let report = compare_rgba(&a, &b, Tolerance::JPEG_TIGHT);

    let failure = tolerance_failure("svs-001 level=0 j2k-vs-reference", &report)
        .expect("failed report should produce gate failure");

    assert!(failure.contains("svs-001 level=0 j2k-vs-reference"));
    assert!(failure.contains("max_abs=2"));
}

#[test]
fn corpus_public_manifest_parses() {
    let path = public_manifest_path();
    let text = std::fs::read_to_string(&path).expect("read public manifest");
    let manifest = parse_manifest(&text).expect("parse public manifest");

    assert!(!manifest.slides.is_empty(), "public manifest has no slides");
    for slide in &manifest.slides {
        assert!(
            slide.redistributable,
            "public entry {} not redistributable",
            slide.alias
        );
        assert!(!slide.alias.is_empty());
        assert!(!slide.format.is_empty());
        assert!(!slide.codecs.is_empty());
    }

    let dicom_jp2k = manifest
        .slides
        .iter()
        .find(|slide| slide.alias == "dicom-jp2k-001")
        .expect("public DICOM JP2K corpus entry");
    assert!(dicom_jp2k.must_decode_level(0));
    assert!(!dicom_jp2k.openslide_must_decode_level(0));

    let svs_jp2k = manifest
        .slides
        .iter()
        .find(|slide| slide.alias == "svs-jp2k-001")
        .expect("public SVS JP2K corpus entry");
    assert!(svs_jp2k.openslide_required);

    for alias in [
        "argos-001",
        "argos-stacked-001",
        "huron-jpeg-001",
        "huron-uncompressed-001",
    ] {
        let entry = manifest
            .slides
            .iter()
            .find(|slide| slide.alias == alias)
            .unwrap_or_else(|| panic!("public {alias} corpus entry"));
        assert!(
            !entry.openslide_required,
            "{alias} must not use OpenSlide's flattened pyramid as its compatibility contract"
        );
        assert!(
            !entry.oracle_divergences.is_empty(),
            "{alias} must document its OpenSlide compatibility boundary"
        );
    }
}

#[test]
fn resolve_present_records_missing_corpus_slides() {
    let entry = |path: &str| support::corpus::CorpusEntry {
        alias: "support-unit-missing-fixture".into(),
        path: path.into(),
        format: "raw_jp2k".into(),
        ..Default::default()
    };
    let mut failures = Vec::new();

    let present = support::corpus::resolve_present(
        &entry("tests/fixtures/jp2k/rgb_nomct.j2k"),
        &mut failures,
    );
    assert!(present.is_some_and(|path| path.is_file()));
    assert!(failures.is_empty());

    let missing = support::corpus::resolve_present(&entry("definitely/missing.j2k"), &mut failures);
    assert_eq!(missing, None);
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].starts_with("support-unit-missing-fixture: corpus slide missing at "),
        "{failures:?}"
    );
}

#[test]
fn oracle_names_are_stable() {
    assert_eq!(J2kOracle.name(), "j2k");
    assert_eq!(ReferenceOracle.name(), "reference");
}

#[test]
fn oracle_reference_unsupported_decode_is_an_error() {
    let err = require_reference_tile(
        Err(ReferenceTileError::unsupported(
            "fixture format is not TIFF JPEG",
        )),
        "fixture level=0 tile=(0,0)",
    )
    .expect_err("unsupported reference decode must not fall back to production");

    assert!(err.contains("reference oracle unsupported"));
    assert!(err.contains("fixture format is not TIFF JPEG"));
    assert!(is_reference_oracle_unsupported(&err));
}

#[test]
fn oracle_sample_buffer_to_rgba_respects_planar_rgb_layout() {
    let tile = CpuTile::new(
        2,
        1,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Planar,
        CpuTileData::u8(vec![10, 40, 20, 50, 30, 60]),
    )
    .expect("valid planar RGB tile");

    let out = sample_buffer_to_rgba(tile).expect("convert");

    assert_eq!(out.pixels_rgba, vec![10, 20, 30, 255, 40, 50, 60, 255]);
}

#[test]
fn oracle_top_left_probe_falls_back_to_region_for_irregular_layout() {
    let slide = OpenedSlide {
        path: PathBuf::from("fixture.bif"),
        oracle_name: "fixture",
        level_count: 1,
        level_dimensions: vec![(123, 45)],
        tile_sizes: vec![None],
        probe_regions: vec![None],
        reader: Box::new(|_, _, _, _, _| Err("tile reader should not be used".into())),
        region_reader: Box::new(|level, x, y, width, height| {
            assert_eq!(level, 0);
            assert_eq!((x, y), (0, 0));
            assert_eq!((width, height), (123, 45));
            Ok(TileBuffer {
                pixels_rgba: vec![0; width as usize * height as usize * 4],
                width,
                height,
            })
        }),
    };

    let probe = top_left_probe(&slide, 0).expect("probe");

    assert_eq!(probe.kind, ProbeKind::Region);
    assert_eq!((probe.width, probe.height), (123, 45));
    let tile = read_probe(&slide, probe).expect("read probe");
    assert_eq!((tile.width, tile.height), (123, 45));
}

#[test]
fn oracle_top_left_probe_clips_virtual_tiles_to_logical_level_geometry() {
    let slide = OpenedSlide {
        path: PathBuf::from("fixture.bif"),
        oracle_name: "fixture",
        level_count: 1,
        level_dimensions: vec![(225, 150)],
        tile_sizes: vec![Some((1024, 1360))],
        probe_regions: vec![None],
        reader: Box::new(|_, _, _, _, _| Err("tile reader should not be used".into())),
        region_reader: Box::new(|_, _, _, width, height| {
            Ok(TileBuffer {
                pixels_rgba: vec![0; width as usize * height as usize * 4],
                width,
                height,
            })
        }),
    };

    let probe = top_left_probe(&slide, 0).expect("probe");

    assert_eq!(probe.kind, ProbeKind::Region);
    assert_eq!((probe.width, probe.height), (225, 150));
}

#[test]
fn oracle_top_left_probe_prefers_sparse_layout_probe_when_available() {
    let slide = OpenedSlide {
        path: PathBuf::from("fixture.mrxs"),
        oracle_name: "fixture",
        level_count: 1,
        level_dimensions: vec![(1000, 1000)],
        tile_sizes: vec![None],
        probe_regions: vec![Some(ProbeRequest {
            level: 0,
            x: 320,
            y: 448,
            width: 128,
            height: 96,
            kind: ProbeKind::Region,
        })],
        reader: Box::new(|_, _, _, _, _| Err("tile reader should not be used".into())),
        region_reader: Box::new(|level, x, y, width, height| {
            assert_eq!(level, 0);
            assert_eq!((x, y), (320, 448));
            assert_eq!((width, height), (128, 96));
            Ok(TileBuffer {
                pixels_rgba: vec![0; width as usize * height as usize * 4],
                width,
                height,
            })
        }),
    };

    let probe = top_left_probe(&slide, 0).expect("probe");

    assert_eq!(probe.kind, ProbeKind::Region);
    assert_eq!((probe.x, probe.y), (320, 448));
    let tile = read_probe(&slide, probe).expect("read probe");
    assert_eq!((tile.width, tile.height), (128, 96));
}

#[cfg(feature = "parity-openslide")]
#[test]
fn openslide_oracle_dimensions_use_bounds_canvas_when_smaller_than_full_canvas() {
    let full_canvas_dims = [(53130, 153470), (13283, 38368), (3321, 9592)];
    let bounds = support::openslide_shim::OpenSlideBounds {
        x: 10778,
        y: 35096,
        width: 36832,
        height: 38432,
    };

    let dims =
        support::oracles::comparable_openslide_level_dimensions(&full_canvas_dims, Some(bounds));

    assert_eq!(dims, vec![(36832, 38432), (9208, 9608), (2302, 2402)]);
    assert!(support::oracles::openslide_level_dimensions_match(
        (2302, 2402),
        full_canvas_dims[2],
        &full_canvas_dims,
        Some(bounds),
    ));
    assert!(!support::oracles::openslide_level_dimensions_match(
        (36832, 38432),
        full_canvas_dims[0],
        &full_canvas_dims,
        None,
    ));
}

#[cfg(feature = "parity-openslide")]
#[test]
fn openslide_oracle_maps_level_coordinates_to_level_zero_world_coordinates() {
    let full_canvas_dims = [(53130, 153470), (13283, 38368), (3321, 9592)];
    let bounds = support::openslide_shim::OpenSlideBounds {
        x: 10778,
        y: 35096,
        width: 36832,
        height: 38432,
    };

    let world = support::oracles::openslide_world_origin_for_probe(
        &full_canvas_dims,
        Some(bounds),
        1,
        256,
        512,
    );

    assert_eq!(world, (11802, 37144));
}

#[cfg(feature = "parity-openslide")]
#[test]
fn openslide_oracle_bounds_canvas_policy_is_vendor_specific() {
    let bounds = support::openslide_shim::OpenSlideBounds {
        x: 10778,
        y: 35096,
        width: 36832,
        height: 38432,
    };

    assert_eq!(
        support::oracles::openslide_comparison_bounds(Some("leica"), Some(bounds)),
        Some(bounds)
    );
    assert_eq!(
        support::oracles::openslide_comparison_bounds(Some("mirax"), Some(bounds)),
        None
    );
    assert_eq!(
        support::oracles::openslide_comparison_bounds(None, Some(bounds)),
        None
    );
}
