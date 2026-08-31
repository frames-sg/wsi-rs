use super::super::*;
use super::fixtures::MiraxFixture;
use crate::core::registry::Slide;

#[test]
fn synthetic_mirax_probes_opens_and_exposes_metadata() {
    let fixture = MiraxFixture::complete();
    let backend = MiraxBackend::new();

    let first = backend.probe(&fixture.path).expect("probe synthetic MIRAX");
    assert!(first.detected);
    assert_eq!(first.vendor, "mirax");
    assert_eq!(first.confidence, ProbeConfidence::Definite);
    assert!(
        backend
            .probe(&fixture.path)
            .expect("repeat MIRAX probe")
            .detected
    );

    let reader = backend.open(&fixture.path).expect("open synthetic MIRAX");
    let dataset = reader.dataset();
    assert_eq!(dataset.properties.vendor(), Some("mirax"));
    assert_eq!(dataset.properties.objective_power(), Some(20.0));
    assert_eq!(dataset.properties.mpp(), Some((0.25, 0.5)));
    assert_eq!(
        dataset.properties.get("openslide.background-color"),
        Some("332211")
    );
    assert_eq!(dataset.properties.get("openslide.bounds-x"), Some("0"));
    assert_eq!(dataset.properties.get("openslide.bounds-y"), Some("0"));
    assert_eq!(dataset.properties.get("openslide.bounds-width"), Some("66"));
    assert_eq!(
        dataset.properties.get("openslide.bounds-height"),
        Some("64")
    );
    let quickhash = dataset
        .properties
        .quickhash1()
        .expect("MIRAX quickhash property");
    assert_eq!(format!("{:032x}", dataset.id.get()), quickhash[..32]);

    let series = &dataset.scenes[0].series[0];
    assert_eq!(series.axes, AxesShape::default());
    assert_eq!(series.sample_type, SampleType::Uint8);
    assert!(series.channels.is_empty());
    assert_eq!(
        series
            .levels
            .iter()
            .map(|level| level.dimensions)
            .collect::<Vec<_>>(),
        vec![(64, 64), (32, 32), (16, 16)]
    );
    assert_eq!(
        series
            .levels
            .iter()
            .map(|level| level.downsample)
            .collect::<Vec<_>>(),
        vec![1.0, 2.0, 4.0]
    );
    for (level, expected_advance) in series.levels.iter().zip([16.0, 8.0, 4.0]) {
        let TileLayout::Irregular {
            tile_advance,
            tiles,
            ..
        } = &level.tile_layout
        else {
            panic!("synthetic MIRAX must expose irregular tiles");
        };
        assert_eq!(*tile_advance, (expected_advance, expected_advance));
        assert_eq!(tiles.len(), 15, "one sparse position is omitted");
        assert!(!tiles.contains_key(&(3, 3)));
    }

    assert_eq!(dataset.associated_images["macro"].dimensions, (12, 8));
    assert_eq!(dataset.associated_images["label"].dimensions, (10, 6));
    assert_eq!(dataset.associated_images["thumbnail"].dimensions, (8, 4));
}

#[test]
fn configured_metadata_limit_rejects_mirax_ini_before_open_allocations() {
    let fixture = MiraxFixture::complete();
    let limits = crate::SlideLimits::default()
        .with_metadata_value_bytes(4)
        .unwrap();
    let backend = MiraxBackend::new();

    let error = match backend.open_with_config(
        &fixture.path,
        BackendOpenConfig::new(CacheConfig::deterministic(), limits),
    ) {
        Ok(_) => panic!("tiny configured metadata limit must reject MIRAX INI values"),
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
fn objective_magnification_is_optional_and_accepts_legacy_x_suffix() {
    let suffixed = MiraxFixture::complete();
    let source = suffixed.complete_slidedat();
    suffixed.write_slidedat(&source.replacen(
        "OBJECTIVE_MAGNIFICATION=20",
        "OBJECTIVE_MAGNIFICATION=20x",
        1,
    ));
    let reader = MiraxBackend::new()
        .open(&suffixed.path)
        .expect("open MIRAX with suffixed objective power");
    assert_eq!(reader.dataset().properties.objective_power(), Some(20.0));

    let missing = MiraxFixture::complete();
    let source = missing.complete_slidedat();
    missing.write_slidedat(&source.replacen("OBJECTIVE_MAGNIFICATION=20\n", "", 1));
    let reader = MiraxBackend::new()
        .open(&missing.path)
        .expect("open MIRAX without objective power");
    assert_eq!(reader.dataset().properties.objective_power(), None);
}

#[test]
fn reader_decodes_jpeg_png_bmp_crops_batches_and_caches() {
    let fixture = MiraxFixture::complete();
    let slide = MiraxSlide::parse(&fixture.path).expect("parse synthetic MIRAX");
    let reader = MiraxReader {
        slide: Arc::new(slide),
    };

    let jpeg = reader
        .read_tile_cpu(&TileRequest::new(0, 0, 0, 0, 0))
        .expect("decode MIRAX JPEG tile");
    assert_eq!((jpeg.width(), jpeg.height()), (16, 16));
    assert_eq!(jpeg.color_space(), &ColorSpace::Rgb);
    assert_eq!(jpeg.layout(), CpuTileLayout::Interleaved);

    let png_left = reader
        .read_tile_cpu(&TileRequest::new(0, 0, 1, 0, 0))
        .expect("decode first cropped MIRAX PNG tile");
    let png_right = reader
        .read_tile_cpu(&TileRequest::new(0, 0, 1, 1, 0))
        .expect("decode second cropped MIRAX PNG tile");
    assert_eq!((png_left.width(), png_left.height()), (8, 8));
    assert_ne!(png_left.as_u8(), png_right.as_u8());
    assert_eq!(
        reader
            .slide
            .decoded_images
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        2,
        "one JPEG image and one shared PNG image are cached"
    );

    let batch = reader
        .read_tiles_cpu(&[
            TileRequest::new(0, 0, 2, 0, 0),
            TileRequest::new(0, 0, 2, 2, 1),
        ])
        .expect("decode MIRAX BMP crop batch");
    let dimensions = batch
        .into_iter()
        .map(|tile| (tile.width(), tile.height()))
        .collect::<Vec<_>>();
    assert_eq!(dimensions, vec![(4, 4), (4, 4)]);

    assert!(reader
        .read_tiles_cpu(&[])
        .expect("empty MIRAX batch")
        .is_empty());
    assert!(matches!(
        reader.read_tile_cpu(&TileRequest::new(0, 0, 0, 3, 3)),
        Err(WsiError::TileRead { .. })
    ));
}

#[test]
fn associated_images_decode_cache_and_report_missing_names() {
    let _serial = super::MIRAX_ASSOCIATED_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = MiraxFixture::complete();
    let slide = MiraxSlide::parse(&fixture.path).expect("parse synthetic MIRAX");

    MIRAX_ASSOCIATED_CACHE_HITS.store(0, Ordering::Relaxed);
    for (name, dimensions) in [
        ("macro", (12, 8)),
        ("label", (10, 6)),
        ("thumbnail", (8, 4)),
    ] {
        let image = slide
            .read_associated(name)
            .expect("decode associated image");
        assert_eq!((image.width(), image.height()), dimensions);
    }
    let first = slide
        .read_associated("thumbnail")
        .expect("read cached associated image");
    let second = slide
        .read_associated("thumbnail")
        .expect("repeat cached associated image");
    assert_eq!(first.as_u8(), second.as_u8());
    assert_eq!(MIRAX_ASSOCIATED_CACHE_HITS.load(Ordering::Relaxed), 2);
    assert!(matches!(
        slide.read_associated("overview"),
        Err(WsiError::AssociatedImageNotFound(name)) if name == "overview"
    ));
    let reader = MiraxReader {
        slide: Arc::new(slide),
    };
    assert_eq!(
        reader
            .read_associated("macro")
            .expect("delegate associated read through SlideReader")
            .width(),
        12
    );
}

#[test]
fn public_slide_composes_region_across_sparse_nonhierarchical_positions() {
    let fixture = MiraxFixture::complete();
    let slide = Slide::open(&fixture.path).expect("open synthetic MIRAX through registry");
    let region = slide
        .read_region(&RegionRequest::new(0, 0, 0, (14, 0), (8, 8)))
        .expect("compose MIRAX region across a position gap");

    assert_eq!((region.width(), region.height()), (8, 8));
    let pixels = region.as_u8().expect("RGB MIRAX region");
    assert_ne!(&pixels[0..3], &[0, 0, 0]);
    assert_eq!(&pixels[2 * 3..3 * 3], &[0, 0, 0]);
    assert_eq!(&pixels[3 * 3..4 * 3], &[0, 0, 0]);
    assert_ne!(&pixels[4 * 3..5 * 3], &[0, 0, 0]);
}

fn open_public_slide() -> (MiraxFixture, Slide) {
    let fixture = MiraxFixture::complete();
    let slide = Slide::open(&fixture.path).expect("open synthetic MIRAX through registry");
    (fixture, slide)
}

fn public_tile_error(slide: &Slide, request: TileRequest) -> WsiError {
    match slide.read_tile(&request) {
        Ok(_) => panic!("invalid MIRAX request unexpectedly read a tile"),
        Err(error) => error,
    }
}

#[test]
fn public_slide_rejects_invalid_mirax_scene_before_later_indices() {
    let (_fixture, slide) = open_public_slide();
    assert!(matches!(
        public_tile_error(&slide, TileRequest::new(1, 1, 3, 0, 0)),
        WsiError::SceneOutOfRange { index: 1, count: 1 }
    ));
}

#[test]
fn public_slide_rejects_invalid_mirax_series_before_level() {
    let (_fixture, slide) = open_public_slide();
    assert!(matches!(
        public_tile_error(&slide, TileRequest::new(0, 1, 3, 0, 0)),
        WsiError::SeriesOutOfRange { index: 1, count: 1 }
    ));
}

#[test]
fn public_slide_rejects_invalid_mirax_level() {
    let (_fixture, slide) = open_public_slide();
    assert!(matches!(
        public_tile_error(&slide, TileRequest::new(0, 0, 3, 0, 0)),
        WsiError::LevelOutOfRange { level: 3, count: 3 }
    ));
}

#[test]
fn public_slide_rejects_invalid_mirax_plane() {
    let (_fixture, slide) = open_public_slide();
    for (plane, expected_axis) in [
        (PlaneSelection { z: 1, c: 0, t: 0 }, "z"),
        (PlaneSelection { z: 0, c: 1, t: 0 }, "c"),
        (PlaneSelection { z: 0, c: 0, t: 1 }, "t"),
    ] {
        let request = TileRequest::new(0, 0, 0, 0, 0).with_plane(plane);
        assert!(matches!(
            public_tile_error(&slide, request),
            WsiError::PlaneOutOfRange {
                axis,
                value: 1,
                max: 0,
            } if axis == expected_axis
        ));
    }
}

#[test]
fn poisoned_private_caches_recover_without_changing_output() {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    let _serial = super::MIRAX_ASSOCIATED_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = MiraxFixture::complete();
    let slide = MiraxSlide::parse(&fixture.path).expect("parse synthetic MIRAX");
    let expected = slide
        .read_associated("macro")
        .expect("prime associated image");
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = slide.associated_cache.lock().unwrap();
        panic!("poison MIRAX associated cache");
    }));
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = slide.decoded_images.lock().unwrap();
        panic!("poison MIRAX decoded image cache");
    }));

    assert_eq!(
        slide
            .read_associated("macro")
            .expect("recover associated cache")
            .as_u8(),
        expected.as_u8()
    );
    let reader = MiraxReader {
        slide: Arc::new(slide),
    };
    assert_eq!(
        reader
            .read_tile_cpu(&TileRequest::new(0, 0, 0, 0, 0))
            .expect("recover decoded image cache")
            .width(),
        16
    );
}

#[test]
fn associated_record_open_errors_retain_the_missing_path() {
    let fixture = MiraxFixture::complete();
    let mut slide = MiraxSlide::parse(&fixture.path).expect("parse synthetic MIRAX");
    let missing = fixture.slide_dir.join("removed-associated.jpg");
    slide.associated.insert(
        "broken".into(),
        MiraxRecord {
            path: missing.clone(),
            offset: 0,
            len: 1,
        },
    );

    assert!(matches!(
        slide.read_associated("broken"),
        Err(WsiError::IoWithPath { path, .. }) if path == missing
    ));
}
