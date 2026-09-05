use super::fixtures::*;
use super::*;
use crate::SlideLimits;
#[test]
fn private_frame_cache_capacity_tracks_cache_config_bytes() {
    let entry_bytes = dicom_frame_cache_entry_bytes(16, 16, 3);
    let mut small_budget = CacheConfig::deterministic()
        .with_shared_tile_bytes(12 * 1024)
        .private_cache_budget(2);
    let mut large_budget = CacheConfig::deterministic()
        .with_shared_tile_bytes(48 * 1024)
        .private_cache_budget(2);
    let small = PrivateCache::<u32, Arc<CpuTile>>::new(small_budget.allocate(entry_bytes));
    let large = PrivateCache::<u32, Arc<CpuTile>>::new(large_budget.allocate(entry_bytes));

    assert_eq!(small.capacity_bytes(), 3 * 1024);
    assert_eq!(large.capacity_bytes(), 12 * 1024);
}

#[test]
fn configured_probe_reuses_the_small_budget_slide_during_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small-budget.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let cache_config = CacheConfig::deterministic().with_shared_tile_bytes(512);
    let config = BackendOpenConfig::new(cache_config, SlideLimits::default());
    let backend = DicomBackend::new();

    let result = backend
        .probe_with_config(&path, config)
        .expect("configured DICOM probe");
    assert!(result.detected);
    let identity = FileIdentity::from_path(&path).unwrap();
    let probed_slide = backend
        .probe_cache
        .get(&identity, config)
        .expect("probe retains the parsed slide for open");
    let image = &probed_slide.levels[0].parts[0];
    assert_eq!(
        image
            .frame_store
            .compressed_frame_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capacity_bytes(),
        128
    );
    assert_eq!(
        image
            .decoded_frame_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capacity_bytes(),
        128
    );

    let reader = backend
        .open_with_config(&path, config)
        .expect("open consumes configured probe result");
    assert_eq!(Arc::strong_count(&probed_slide), 2);
    assert!(backend.probe_cache.get(&identity, config).is_none());
    drop(reader);
    assert_eq!(Arc::strong_count(&probed_slide), 1);
}
#[test]
fn default_backend_supports_unconfigured_probe_and_open_traits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default-backend.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let backend = DicomBackend::default();

    let probe = FormatProbe::probe(&backend, &path).expect("default DICOM probe");
    assert!(probe.detected);
    assert_eq!(probe.vendor, "dicom");

    let reader = DatasetReader::open(&backend, &path).expect("default DICOM open");
    assert_eq!(reader.dataset().scenes[0].series[0].levels.len(), 1);
}

#[test]
fn configured_open_enforces_metadata_value_limit_during_parse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata-limit.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let limits = SlideLimits::default()
        .with_metadata_value_bytes(1)
        .expect("nonzero metadata limit");
    let backend = DicomBackend::new();

    let error = match backend.open_with_config(
        &path,
        BackendOpenConfig::new(CacheConfig::deterministic(), limits),
    ) {
        Ok(_) => panic!("configured parser accepted an oversized metadata value"),
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
