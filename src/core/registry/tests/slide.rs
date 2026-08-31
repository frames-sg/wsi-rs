use super::super::*;
use super::support::MockSource;
use crate::DecodeAcceleration;
use std::sync::Arc;

#[test]
fn conservative_managed_reader_forwards_reads_and_reports_bounds() {
    let encoded_limit = 1_234;
    let reader = ConservativeManagedReader::new(Box::new(MockSource::new()), encoded_limit);
    let tile = TileRequest::new(0, 0, 0, 0, 0);
    let tiles = [tile.clone()];
    let display = TileViewRequest::new(0, 0, 0, 0, 0, 256, 256);
    let region = RegionRequest::new(0, 0, 0, (0, 0), (16, 16));
    let control = crate::ReadControl::default();

    assert_eq!(reader.dataset().scenes.len(), 1);
    assert_eq!(reader.tile_codec_kind(&tile), TileCodecKind::Other);
    assert!(matches!(
        reader.level_source_kind(0usize.into(), 0usize.into(), 0u32.into()),
        Ok(LevelSourceKind::Physical)
    ));
    reader
        .prepare_level_controlled(0usize.into(), 0usize.into(), 0u32.into(), &control)
        .unwrap();
    assert_eq!(reader.read_tile_cpu(&tile).unwrap().width(), 256);
    assert_eq!(reader.read_tiles_cpu(&tiles).unwrap().len(), 1);
    assert_eq!(
        reader
            .read_tiles_cpu_controlled(&tiles, &control)
            .unwrap()
            .len(),
        1
    );
    assert!(reader.read_raw_compressed_tile(&tile).is_err());
    assert!(reader.read_raw_compressed_display_tile(&display).is_err());
    assert!(reader.use_display_tile_cache(&display));
    let mut context = SlideReadContext::new(None, crate::SlideLimits::default().region_pixels());
    assert!(reader.read_region_fastpath(&mut context, &region).is_none());
    assert_eq!(reader.read_region(&region).unwrap().width(), 16);
    assert_eq!(reader.read_display_tile(&display).unwrap().width(), 256);
    assert!(matches!(
        reader.read_associated("label"),
        Err(WsiError::AssociatedImageNotFound(_))
    ));

    assert_eq!(
        reader.tile_encoded_upper_bound(&tile).unwrap(),
        encoded_limit
    );
    assert_eq!(reader.tile_batch_encoded_upper_bound(&[]).unwrap(), 0);
    assert_eq!(
        reader.tile_batch_encoded_upper_bound(&tiles).unwrap(),
        encoded_limit
    );
    assert_eq!(
        reader.display_tile_encoded_upper_bound(&display).unwrap(),
        encoded_limit
    );
    assert_eq!(
        reader.associated_encoded_upper_bound("label").unwrap(),
        encoded_limit
    );
    assert_eq!(
        reader.region_fastpath_encoded_upper_bound(&region).unwrap(),
        encoded_limit
    );
}

struct IrregularCountingSource {
    dataset: Dataset,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

struct RawCountingSource {
    dataset: Dataset,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

impl SlideReader for RawCountingSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        unreachable!("raw admission test must not request decoded pixels")
    }

    fn read_raw_compressed_tile(&self, _req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        unreachable!("oversized promised input must fail before source work")
    }
}

impl SlideReader for IrregularCountingSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(WsiError::TileRead {
            level: 0,
            col: 1,
            row: 0,
            reason: "tile not found".into(),
        })
    }
}

#[test]
fn slide_reader_defaults_to_no_associated_images() {
    let error = MockSource::new()
        .read_associated("label")
        .expect_err("a reader without associated images should report not found");

    assert!(matches!(
        error,
        WsiError::AssociatedImageNotFound(name) if name == "label"
    ));
}

#[test]
fn slide_open_options_default_disables_implicit_svcache_resolution() {
    let options = SlideOpenOptions::default();

    assert_eq!(
        options.svcache_policy,
        crate::formats::svcache::SvcachePolicy::Off
    );
    assert_eq!(options.cache_config, CacheConfig::deterministic());
}

#[test]
fn slide_open_options_configures_decode_execution() {
    let options = SlideOpenOptions::default().with_decode_execution_options(
        DecodeExecutionOptions::default().with_acceleration(DecodeAcceleration::CpuOnly),
    );

    assert_eq!(
        options.decode_execution_options().acceleration(),
        DecodeAcceleration::CpuOnly
    );
}

#[test]
fn slide_open_options_expose_cache_svcache_and_region_limits() {
    let cache = CacheConfig::deterministic().with_shared_tile_bytes(8192);
    let options = SlideOpenOptions::deterministic()
        .with_cache_config(cache)
        .with_svcache_policy(crate::formats::svcache::SvcachePolicy::PreferFresh)
        .with_max_region_pixels(1234);

    assert_eq!(options.cache_config(), cache);
    assert_eq!(
        options.svcache_policy(),
        crate::formats::svcache::SvcachePolicy::PreferFresh
    );
    assert_eq!(options.max_region_pixels(), 1234);
}

#[test]
fn slide_reader_context_and_default_boundaries_are_observable() {
    let context = SlideReadContext::new(None, 321);
    assert_eq!(context.max_region_pixels(), 321);

    let source = MockSource::new();
    let tile = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    assert_eq!(source.tile_codec_kind(&tile), TileCodecKind::Other);
    source
        .prepare_level_controlled(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(0),
            &crate::ReadControl::default(),
        )
        .expect("default level preparation");

    let view = TileViewRequest::new(0usize, 0usize, 0u32, 0, 0, 256, 256);
    let display = source
        .read_display_tile(&view)
        .expect("default display composition");
    assert_eq!(&display.as_u8().unwrap()[..3], &[255, 0, 0]);
    assert!(source.associated_image("label").unwrap().is_none());
}

#[test]
fn slide_exposes_dataset() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = std::sync::Arc::new(TileCache::new(1024 * 1024));
    let handle = Slide::from_source(source, cache);

    assert_eq!(handle.dataset().id, DatasetId::new(1));
    assert_eq!(handle.dataset().scenes.len(), 1);
    assert_eq!(
        handle.dataset().scenes[0].series[0].levels[0].dimensions,
        (512, 512)
    );
}

#[test]
fn slide_level_source_kind_accepts_plain_indices() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = std::sync::Arc::new(TileCache::new(1024 * 1024));
    let handle = Slide::from_source(source, cache);

    assert_eq!(
        handle.level_source_kind(0usize, 0usize, 0u32).unwrap(),
        LevelSourceKind::Physical
    );
}

#[test]
fn slide_reader_defaults_return_explicit_metadata_errors() {
    let source = MockSource::new();
    assert!(matches!(
        source.level_source_kind(SceneId::new(0), SeriesId::new(0), LevelIdx::new(7)),
        Err(WsiError::LevelOutOfRange { level: 7, .. })
    ));

    let tile = TileRequest::new(0usize, 0usize, 0u32, 3, 4);
    let err = source.read_raw_compressed_tile(&tile).unwrap_err();
    assert!(err
        .to_string()
        .contains("raw compressed tile access is not available"));
    assert!(err.to_string().contains("tile (3, 4) at level 0"));

    let view = TileViewRequest::new(0usize, 0usize, 0u32, 5, 6, 256, 256);
    let err = source.read_raw_compressed_display_tile(&view).unwrap_err();
    assert!(err
        .to_string()
        .contains("raw compressed display tile access is not available"));
    assert!(err.to_string().contains("tile (5, 6) at level 0"));
}

#[test]
fn read_associated_delegates_to_source() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(1024));
    let handle = Slide::from_source(source, cache);

    match handle.read_associated("label") {
        Err(WsiError::AssociatedImageNotFound(name)) => {
            assert_eq!(name, "label");
        }
        other => panic!("expected AssociatedImageNotFound, got {:?}", other),
    }
}

#[test]
fn missing_irregular_tile_is_rejected_before_backend_invocation() {
    let mut dataset = crate::test_support::regular_rgb_dataset_for_test(
        DatasetId::new(991),
        "s0",
        "ser0",
        crate::test_support::RegularLevelForTest {
            dimensions: (512, 256),
            tile_width: 256,
            tile_height: 256,
            tiles_across: 2,
            tiles_down: 1,
        },
    );
    dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::Irregular {
        tile_advance: (256.0, 256.0),
        extra_tiles: (0, 0, 0, 0),
        tiles: [((0, 0), TileEntry::new((0.0, 0.0), (256, 256)))]
            .into_iter()
            .collect(),
    };
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let slide = Slide::from_source(
        Box::new(IrregularCountingSource {
            dataset,
            reads: Arc::clone(&reads),
        }),
        Arc::new(TileCache::new(1024)),
    );

    assert!(matches!(
        slide.read_tile(&TileRequest::new(0usize, 0usize, 0, 1, 0)),
        Err(WsiError::TileRead {
            level: 0,
            col: 1,
            row: 0,
            ..
        })
    ));
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn raw_compressed_admission_reserves_before_backend_invocation() {
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let limits = SlideLimits::default()
        .with_encoded_unit_bytes(1024)
        .unwrap()
        .with_operation_transient_bytes(512)
        .unwrap();
    let source = RawCountingSource {
        dataset: crate::test_support::regular_rgb_dataset_for_test(
            DatasetId::new(992),
            "s0",
            "ser0",
            crate::test_support::RegularLevelForTest {
                dimensions: (1, 1),
                tile_width: 1,
                tile_height: 1,
                tiles_across: 1,
                tiles_down: 1,
            },
        ),
        reads: Arc::clone(&reads),
    };
    let slide = Slide::from_source_with_config_and_runtime(
        Box::new(source),
        CacheConfig::deterministic(),
        limits,
        DecodeRuntime::default_arc(),
    );

    assert!(matches!(
        slide.read_raw_compressed_tile(&TileRequest::new(0usize, 0usize, 0, 0, 0)),
        Err(WsiError::ResourceLimit {
            resource: "per-operation transient work",
            requested: 1024,
            limit: 512,
        })
    ));
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn encoded_payload_exceeding_promised_bound_is_a_backend_contract_error() {
    let slide = Slide::from_source(Box::new(MockSource::new()), Arc::new(TileCache::new(1024)));

    assert!(matches!(
        slide.validate_encoded_contract(9, 8, "test encoded payload"),
        Err(WsiError::BackendContract {
            context: "test encoded payload",
            expected: 8,
            actual: 9,
        })
    ));
}
