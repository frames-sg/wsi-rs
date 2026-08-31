use super::*;
use crate::core::types::*;

const SVS_RGB_240_TILE_BYTES: usize = 240 * 240 * 3;
const COMMON_ZOOM_VIEWPORT_TILE_COUNT: i64 = 96;

fn make_sample_buffer(size: usize) -> CpuTile {
    CpuTile {
        width: 256,
        height: 256,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![0u8; size]),
    }
}

fn make_key(dataset_id: u128, level: u32, col: i64, row: i64) -> CacheKey {
    CacheKey {
        dataset_id: DatasetId::new(dataset_id),
        scene: 0,
        series: 0,
        level,
        z: 0,
        c: 0,
        t: 0,
        tile_col: col,
        tile_row: row,
    }
}

#[test]
fn cache_key_from_tile_request_preserves_every_identity_dimension() {
    let request = TileRequest {
        scene: 11usize.into(),
        series: 12usize.into(),
        level: 13u32.into(),
        plane: PlaneSelection {
            z: 14,
            c: 15,
            t: 16,
        }
        .into(),
        col: -17,
        row: 18,
    };

    assert_eq!(
        CacheKey::from_tile_request(DatasetId::new(19), &request),
        CacheKey {
            dataset_id: DatasetId::new(19),
            scene: 11,
            series: 12,
            level: 13,
            z: 14,
            c: 15,
            t: 16,
            tile_col: -17,
            tile_row: 18,
        }
    );
}

#[test]
fn private_cache_budget_is_byte_weighted_and_bounds_aggregate_capacity() {
    let config = CacheConfig::deterministic().with_shared_tile_bytes(8 * 1024);
    let mut budget = config.private_cache_budget(8);
    let mut caches = (0..8)
        .map(|_| PrivateCache::<u32, u32>::new(budget.allocate(1024)))
        .collect::<Vec<_>>();

    assert!(
        caches
            .iter()
            .map(PrivateCache::accounted_capacity_bytes)
            .sum::<u64>()
            <= config.private_cache_budget_bytes()
    );
    assert_eq!(
        caches
            .iter()
            .map(PrivateCache::accounted_capacity_bytes)
            .sum::<u64>(),
        config.private_cache_budget_bytes()
    );

    let cache = &mut caches[0];
    let capacity = cache.accounted_capacity_bytes();
    cache.put(
        1,
        2,
        capacity.saturating_sub(PRIVATE_CACHE_ENTRY_ACCOUNTING_FLOOR_BYTES),
    );
    cache.put(2, 3, 1);
    assert!(cache.current_bytes() <= capacity);
    assert_eq!(cache.len(), 1, "byte overflow evicts the older entry");
}

#[test]
fn deterministic_default_cache_split_is_sixty_four_thirty_two_thirty_two_mib() {
    let config = CacheConfig::deterministic();

    assert_eq!(config.shared_tile_budget(Some(1)), 64 * 1024 * 1024);
    assert_eq!(config.display_tile_budget(), 32 * 1024 * 1024);
    assert_eq!(config.private_cache_budget_bytes(), 32 * 1024 * 1024);
}

#[test]
fn put_and_get() {
    let cache = TileCache::new(1024 * 1024);
    let buf = Arc::new(make_sample_buffer(100));
    let key = make_key(1, 0, 0, 0);
    cache.put(key.clone(), buf.clone());
    let result = cache.get(&key).unwrap();
    assert_eq!(result.width, 256);
}

#[test]
fn miss_returns_none() {
    let cache = TileCache::new(1024);
    let key = make_key(1, 0, 0, 0);
    assert!(cache.get(&key).is_none());
}

#[test]
fn eviction_by_byte_size() {
    let cache = TileCache::new(250);
    cache.put(make_key(1, 0, 0, 0), Arc::new(make_sample_buffer(100)));
    cache.put(make_key(1, 0, 1, 0), Arc::new(make_sample_buffer(100)));
    assert!(cache.get(&make_key(1, 0, 0, 0)).is_some());
    assert!(cache.get(&make_key(1, 0, 1, 0)).is_some());

    cache.put(make_key(1, 0, 2, 0), Arc::new(make_sample_buffer(100)));
    assert!(cache.get(&make_key(1, 0, 0, 0)).is_none());
    assert!(cache.get(&make_key(1, 0, 1, 0)).is_some());
    assert!(cache.get(&make_key(1, 0, 2, 0)).is_some());
}

#[test]
fn different_datasets_are_independent() {
    let cache = TileCache::new(1024);
    cache.put(make_key(1, 0, 0, 0), Arc::new(make_sample_buffer(10)));
    cache.put(make_key(2, 0, 0, 0), Arc::new(make_sample_buffer(10)));
    assert!(cache.get(&make_key(1, 0, 0, 0)).is_some());
    assert!(cache.get(&make_key(2, 0, 0, 0)).is_some());
}

#[test]
fn axis_aware_keys() {
    let cache = TileCache::new(1024);
    let mut key_z0 = make_key(1, 0, 0, 0);
    key_z0.z = 0;
    let mut key_z1 = make_key(1, 0, 0, 0);
    key_z1.z = 1;
    cache.put(key_z0.clone(), Arc::new(make_sample_buffer(10)));
    cache.put(key_z1.clone(), Arc::new(make_sample_buffer(10)));
    assert!(cache.get(&key_z0).is_some());
    assert!(cache.get(&key_z1).is_some());
}

#[test]
fn oversize_entry_rejected() {
    let cache = TileCache::new(50);
    cache.put(make_key(1, 0, 0, 0), Arc::new(make_sample_buffer(100)));
    assert!(cache.get(&make_key(1, 0, 0, 0)).is_none());
}

#[test]
fn shared_across_threads() {
    let cache = Arc::new(TileCache::new(4096));
    let cache_clone = cache.clone();
    let handle = std::thread::spawn(move || {
        cache_clone.put(make_key(1, 0, 5, 5), Arc::new(make_sample_buffer(10)));
    });
    handle.join().unwrap();
    assert!(cache.get(&make_key(1, 0, 5, 5)).is_some());
}

#[test]
fn display_default_holds_common_svs_zoom_viewport_working_set() {
    let cache = TileCache::new(DEFAULT_DISPLAY_TILE_CACHE_SIZE);
    for col in 0..COMMON_ZOOM_VIEWPORT_TILE_COUNT {
        cache.put(
            make_key(1, 0, col, 0),
            Arc::new(make_sample_buffer(SVS_RGB_240_TILE_BYTES)),
        );
    }

    let stats = cache.stats();
    assert_eq!(stats.entries, COMMON_ZOOM_VIEWPORT_TILE_COUNT as usize);
    assert_eq!(stats.evictions, 0);
    assert_eq!(stats.rejected_oversize, 0);
}

#[test]
fn stats_count_hits_misses_puts_evictions_and_oversize_rejections() {
    let cache = TileCache::new(150);
    let missing = make_key(1, 0, 9, 9);
    assert!(cache.get(&missing).is_none());

    cache.put(make_key(1, 0, 0, 0), Arc::new(make_sample_buffer(100)));
    assert!(cache.get(&make_key(1, 0, 0, 0)).is_some());

    cache.put(make_key(1, 0, 1, 0), Arc::new(make_sample_buffer(100)));
    cache.put(make_key(1, 0, 2, 0), Arc::new(make_sample_buffer(200)));

    let stats = cache.stats();
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.puts, 2);
    assert_eq!(stats.evictions, 1);
    assert_eq!(stats.rejected_oversize, 1);
    assert_eq!(stats.capacity_bytes, 150);
    assert_eq!(stats.current_bytes, 100);
    assert_eq!(stats.entries, 1);
}

#[test]
fn cache_configuration_defaults_and_poison_recovery_cover_every_public_operation() {
    let config = CacheConfig::default().with_display_tile_bytes(321);
    assert_eq!(config.display_tile_budget(), 321);
    assert_eq!(
        TileCache::shared_default_with_hint(123)
            .stats()
            .capacity_bytes,
        123
    );
    assert_eq!(
        TileCache::default().stats().capacity_bytes,
        DEFAULT_TILE_CACHE_SIZE
    );

    let cache = Arc::new(TileCache::new(1_024));
    let poisoner = Arc::clone(&cache);
    assert!(std::thread::spawn(move || {
        let _state = poisoner.inner.lock().unwrap();
        panic!("poison tile cache state");
    })
    .join()
    .is_err());

    assert!(format!("{cache:?}").contains("TileCache"));
    let key = make_key(7, 0, 0, 0);
    cache.put(key.clone(), Arc::new(make_sample_buffer(10)));
    assert!(cache.get(&key).is_some());
    assert_eq!(cache.stats().entries, 1);
}
