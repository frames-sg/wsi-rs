use super::fixtures::*;
use super::runtime::*;
use super::*;
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

    assert_eq!(small.capacity_entries(), 1);
    assert_eq!(large.capacity_entries(), 4);
}

#[test]
fn configured_probe_reuses_the_small_budget_slide_during_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small-budget.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let cache_config = CacheConfig::deterministic().with_shared_tile_bytes(512);
    let backend = DicomBackend::new();

    let result = backend
        .probe_with_cache_config(&path, cache_config)
        .expect("configured DICOM probe");
    assert!(result.detected);
    let identity = FileIdentity::from_path(&path).unwrap();
    let probed_slide = backend
        .probe_cache
        .get(&identity, cache_config)
        .expect("probe retains the parsed slide for open");
    let image = &probed_slide.levels[0].parts[0];
    assert_eq!(
        image
            .frame_store
            .compressed_frame_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capacity_entries(),
        0
    );
    assert_eq!(
        image
            .decoded_frame_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capacity_entries(),
        0
    );

    let reader = backend
        .open_with_cache_config(&path, cache_config)
        .expect("open consumes configured probe result");
    assert_eq!(Arc::strong_count(&probed_slide), 2);
    assert!(backend.probe_cache.get(&identity, cache_config).is_none());
    drop(reader);
    assert_eq!(Arc::strong_count(&probed_slide), 1);
}
#[test]
fn compressed_dicom_default_cache_covers_common_read_region_working_set() {
    let levels = build_levels(
        Path::new("cache-hint.dcm"),
        vec![test_dicom_image_with_transfer_syntax(
            "1.2.3.1",
            DicomGrid::Full,
            uids::JPEG2000_LOSSLESS,
        )],
    )
    .expect("level should build");
    let working_set_bytes = levels[0]
        .cache_bytes_for_target_region()
        .expect("compressed DICOM working set should be computable");
    assert_eq!(working_set_bytes, 12 * 1024 * 1024);
    assert!(crate::core::cache::DEFAULT_TILE_CACHE_SIZE >= working_set_bytes);

    let reader = DicomReader {
        slide: Arc::new(DicomSlide {
            dataset: empty_dataset(),
            levels,
            associated: HashMap::new(),
        }),
    };

    assert_eq!(reader.recommended_shared_cache_bytes(), None);
}

#[test]
fn native_dicom_keeps_default_shared_cache_hint() {
    let levels = build_levels(
        Path::new("native-cache-hint.dcm"),
        vec![test_dicom_image_with_transfer_syntax(
            "1.2.3.1",
            DicomGrid::Full,
            uids::EXPLICIT_VR_LITTLE_ENDIAN,
        )],
    )
    .expect("level should build");
    let reader = DicomReader {
        slide: Arc::new(DicomSlide {
            dataset: empty_dataset(),
            levels,
            associated: HashMap::new(),
        }),
    };

    assert_eq!(reader.recommended_shared_cache_bytes(), None);
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
