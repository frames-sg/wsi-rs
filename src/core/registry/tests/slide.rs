use super::super::*;
use super::support::MockSource;
use std::sync::Arc;

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
    let options = SlideOpenOptions::default()
        .with_decode_execution_options(DecodeExecutionOptions::default().with_route_sample_size(2));

    assert_eq!(options.decode_execution_options().route_sample_size(), 2);
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
    let output = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    let context = SlideReadContext::new(None, output.clone(), 321);
    assert!(matches!(
        context.output(),
        TileOutputPreference::PreferDevice { .. }
    ));
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
fn adaptive_route_cpu_wins_when_device_is_slower() {
    let winner = DecodeRouteDecision::winner_for_measurement(
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(20),
        4,
    );

    assert_eq!(winner, DecodeRoute::Cpu);
}

#[test]
fn adaptive_route_cpu_wins_when_device_returns_no_resident_tiles() {
    let winner = DecodeRouteDecision::winner_for_measurement(
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(1),
        0,
    );

    assert_eq!(winner, DecodeRoute::Cpu);
}

#[test]
fn adaptive_route_device_wins_when_it_beats_threshold() {
    let winner = DecodeRouteDecision::winner_for_measurement(
        std::time::Duration::from_millis(100),
        std::time::Duration::from_millis(80),
        4,
    );

    assert_eq!(winner, DecodeRoute::Device);
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
