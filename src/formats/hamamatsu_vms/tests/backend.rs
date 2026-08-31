use std::fs;

use super::super::*;
use super::fixtures::VmsFixture;
use crate::core::registry::Slide;
use crate::formats::hamamatsu_vms::model::VmsLevel;

#[test]
fn complete_vms_probes_opens_and_exposes_normalized_metadata() {
    let fixture = VmsFixture::complete();
    let backend = HamamatsuVmsBackend::new();

    let first_probe = backend.probe(&fixture.path).expect("probe synthetic VMS");
    assert!(first_probe.detected);
    assert_eq!(first_probe.vendor, "hamamatsu");
    assert_eq!(first_probe.confidence, ProbeConfidence::Definite);
    assert!(
        backend
            .probe(&fixture.path)
            .expect("repeat cached VMS probe")
            .detected
    );

    let reader = backend
        .open(&fixture.path)
        .expect("open cached synthetic VMS");
    let dataset = reader.dataset();
    let quickhash = dataset
        .properties
        .quickhash1()
        .expect("VMS quickhash property");
    assert_eq!(format!("{:032x}", dataset.id.get()), quickhash[..32]);
    assert_eq!(dataset.properties.vendor(), Some("hamamatsu"));
    assert_eq!(dataset.properties.objective_power(), Some(40.0));
    assert_eq!(dataset.properties.mpp(), Some((1.0, 1.0)));
    assert_eq!(
        dataset.properties.get("openslide.comment"),
        Some("synthetic VMS comment")
    );
    assert_eq!(
        dataset.properties.get("hamamatsu.Reference"),
        Some("synthetic")
    );

    let series = &dataset.scenes[0].series[0];
    assert_eq!(series.axes, AxesShape::default());
    assert_eq!(series.sample_type, SampleType::Uint8);
    assert!(series.channels.is_empty());
    let dimensions: Vec<_> = series.levels.iter().map(|level| level.dimensions).collect();
    assert_eq!(
        dimensions,
        vec![(256, 16), (128, 8), (64, 8), (32, 4), (16, 2), (8, 1)]
    );
    assert_eq!(series.levels[0].downsample, 1.0);
    assert_eq!(series.levels[5].downsample, 32.0);
    assert!(matches!(
        series.levels[0].tile_layout,
        TileLayout::Regular {
            tile_width: 64,
            tile_height: 8,
            tiles_across: 4,
            tiles_down: 2,
        }
    ));
    let macro_image = dataset
        .associated_images
        .get("macro")
        .expect("VMS macro metadata");
    assert_eq!(macro_image.dimensions, (24, 16));
    assert_eq!(macro_image.sample_type, SampleType::Uint8);
    assert_eq!(macro_image.channels, 3);
}

#[test]
fn configured_metadata_limit_rejects_vms_ini_before_open_allocations() {
    let fixture = VmsFixture::complete();
    let limits = crate::SlideLimits::default()
        .with_metadata_value_bytes(4)
        .unwrap();
    let backend = HamamatsuVmsBackend::new();

    let error = match backend.open_with_config(
        &fixture.path,
        BackendOpenConfig::new(CacheConfig::deterministic(), limits),
    ) {
        Ok(_) => panic!("tiny configured metadata limit must reject VMS INI values"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        WsiError::ResourceLimit {
            resource: "individual metadata value",
            ..
        }
    ));
}

#[test]
fn backend_default_and_empty_level_return_defined_results() {
    let _backend = HamamatsuVmsBackend::default();
    assert!(matches!(
        VmsLevel::new(Vec::new(), 1, 1, 1),
        Err(WsiError::InvalidSlide { message, .. }) if message.contains("no JPEG shards")
    ));
}

#[test]
fn vms_reader_decodes_tiles_batches_scales_and_associated_image_cache() {
    let fixture = VmsFixture::complete();
    let reader = HamamatsuVmsBackend::new()
        .open(&fixture.path)
        .expect("open synthetic VMS");

    let first_request = TileRequest::new(0, 0, 0, 0, 0);
    let first = reader
        .read_tile_cpu(&first_request)
        .expect("read first VMS tile");
    assert_eq!((first.width(), first.height()), (64, 8));
    assert_eq!(first.channels(), 3);
    assert_eq!(first.color_space(), &ColorSpace::Rgb);
    assert_eq!(first.layout(), CpuTileLayout::Interleaved);

    let second_shard = reader
        .read_tile_cpu(&TileRequest::new(0, 0, 0, 2, 0))
        .expect("read tile from second VMS shard");
    assert_ne!(first.as_u8(), second_shard.as_u8());

    let cached = reader
        .read_tile_cpu(&first_request)
        .expect("repeat cached VMS tile");
    assert_eq!(cached.as_u8(), first.as_u8());

    let batch = reader
        .read_tiles_cpu(&[
            TileRequest::new(0, 0, 1, 0, 0),
            TileRequest::new(0, 0, 3, 0, 0),
            TileRequest::new(0, 0, 4, 0, 0),
            TileRequest::new(0, 0, 5, 0, 0),
        ])
        .expect("read scaled VMS batch");
    let dimensions: Vec<_> = batch
        .into_iter()
        .map(|tile| (tile.width(), tile.height()))
        .collect();
    assert_eq!(dimensions, vec![(32, 4), (32, 4), (16, 2), (8, 1)]);

    let macro_first = reader.read_associated("macro").expect("decode VMS macro");
    let macro_cached = reader
        .read_associated("macro")
        .expect("read cached VMS macro");
    assert_eq!((macro_first.width(), macro_first.height()), (24, 16));
    assert_eq!(macro_first.as_u8(), macro_cached.as_u8());
    assert!(matches!(
        reader.read_associated("label"),
        Err(WsiError::AssociatedImageNotFound(name)) if name == "label"
    ));
}

#[test]
fn vms_reader_rejects_signed_and_grid_tile_bounds() {
    let fixture = VmsFixture::complete();
    let reader = HamamatsuVmsBackend::new()
        .open(&fixture.path)
        .expect("open synthetic VMS");

    for request in [
        TileRequest::new(0, 0, 0, -1, 0),
        TileRequest::new(0, 0, 0, 4, 0),
        TileRequest::new(0, 0, 0, 0, -1),
        TileRequest::new(0, 0, 0, 0, 2),
    ] {
        assert!(matches!(
            reader.read_tile_cpu(&request),
            Err(WsiError::TileRead { .. })
        ));
    }
}

fn open_public_slide() -> (VmsFixture, Slide) {
    let fixture = VmsFixture::complete();
    let slide = Slide::open(&fixture.path).expect("open synthetic VMS through registry");
    (fixture, slide)
}

fn public_tile_error(slide: &Slide, request: TileRequest) -> WsiError {
    match slide.read_tile(&request) {
        Ok(_) => panic!("invalid VMS index unexpectedly read a tile"),
        Err(error) => error,
    }
}

#[test]
fn slide_reads_valid_vms_tile() {
    let (_fixture, slide) = open_public_slide();
    let valid = slide
        .read_tile(&TileRequest::new(0, 0, 0, 0, 0))
        .expect("read valid VMS tile through public Slide boundary");
    assert_eq!((valid.width(), valid.height()), (64, 8));
}

#[test]
fn slide_rejects_invalid_vms_scene() {
    let (_fixture, slide) = open_public_slide();
    let scene_error = public_tile_error(&slide, TileRequest::new(1, 1, 6, 0, 0));
    assert!(matches!(
        scene_error,
        WsiError::SceneOutOfRange { index: 1, count: 1 }
    ));
}

#[test]
fn slide_rejects_invalid_vms_series() {
    let (_fixture, slide) = open_public_slide();
    let series_error = public_tile_error(&slide, TileRequest::new(0, 1, 6, 0, 0));
    assert!(matches!(
        series_error,
        WsiError::SeriesOutOfRange { index: 1, count: 1 }
    ));
}

#[test]
fn slide_rejects_invalid_vms_level() {
    let (_fixture, slide) = open_public_slide();
    let level_error = public_tile_error(&slide, TileRequest::new(0, 0, 6, 0, 0));
    assert!(matches!(
        level_error,
        WsiError::LevelOutOfRange { level: 6, count: 6 }
    ));
}

#[test]
fn slide_composes_region_across_vms_jpeg_shards() {
    let fixture = VmsFixture::complete();
    let slide = Slide::open(&fixture.path).expect("open synthetic VMS through registry");
    let region = slide
        .read_region(&RegionRequest::new(0, 0, 0, (124, 0), (8, 8)))
        .expect("compose VMS region across shard boundary");

    assert_eq!((region.width(), region.height()), (8, 8));
    let pixels = region.as_u8().expect("RGB VMS region");
    assert_ne!(&pixels[0..3], &pixels[4 * 3..5 * 3]);
}

#[test]
fn associated_reads_recover_poisoned_cache_and_report_removed_source() {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    let fixture = VmsFixture::complete();
    let reader = HamamatsuVmsBackend::new()
        .open(&fixture.path)
        .expect("open synthetic VMS");

    let slide = VmsSlide::parse_with_cache_config(&fixture.path, CacheConfig::deterministic())
        .expect("parse mutable VMS slide");
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = slide.associated_cache.lock().unwrap();
        panic!("poison associated cache");
    }));
    let poisoned_reader = VmsReader {
        slide: Arc::new(slide),
    };
    assert_eq!(
        poisoned_reader
            .read_associated("macro")
            .expect("recover poisoned associated cache")
            .width(),
        24
    );

    fs::remove_file(&fixture.macro_path).expect("remove associated source after parse");
    assert!(matches!(
        reader.read_associated("macro"),
        Err(WsiError::IoWithPath { .. })
    ));
}
