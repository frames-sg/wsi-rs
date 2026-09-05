use super::*;

fn make_sample_buffer(size: usize) -> CpuTile {
    CpuTile {
        width: 64,
        height: 64,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![0u8; size]),
    }
}

#[test]
fn full_decode_cache_put_and_get() {
    let cache = FullDecodeCache::new(1024);
    let buf = Arc::new(make_sample_buffer(100));
    cache.put(IfdId(1000), buf.clone());

    let result = cache.get(&IfdId(1000));
    assert!(result.is_some());
    assert_eq!(result.unwrap().width, 64);
}

#[test]
fn full_decode_cache_eviction() {
    let cache = FullDecodeCache::new(250);
    cache.put(IfdId(100), Arc::new(make_sample_buffer(100)));
    cache.put(IfdId(200), Arc::new(make_sample_buffer(100)));
    // 200 bytes used — both fit
    assert!(cache.get(&IfdId(100)).is_some());
    assert!(cache.get(&IfdId(200)).is_some());

    // Third entry pushes over 250 — LRU (IfdId(100)) should be evicted
    // Note: after the two gets above, access order is 100 then 200,
    // so IfdId(100) is older. But LruCache.get() promotes, so after
    // get(100) then get(200), 100 was accessed first, then 200.
    // The LRU is IfdId(100).
    cache.put(IfdId(300), Arc::new(make_sample_buffer(100)));
    assert!(cache.get(&IfdId(100)).is_none()); // evicted
    assert!(cache.get(&IfdId(200)).is_some());
    assert!(cache.get(&IfdId(300)).is_some());
}

#[test]
fn full_decode_cache_oversize_rejected() {
    let cache = FullDecodeCache::new(50);
    let buf = Arc::new(make_sample_buffer(100));
    cache.put(IfdId(1000), buf);

    assert!(cache.get(&IfdId(1000)).is_none());
    assert_eq!(cache.current_bytes(), 0);
}

#[test]
fn full_decode_cache_miss() {
    let cache = FullDecodeCache::new(1024);
    assert!(cache.get(&IfdId(9999)).is_none());
}

#[test]
fn full_decode_cache_replacement_updates_bytes() {
    let cache = FullDecodeCache::new(500);
    cache.put(IfdId(100), Arc::new(make_sample_buffer(100)));
    assert_eq!(cache.current_bytes(), 100);

    // Replace with larger buffer
    cache.put(IfdId(100), Arc::new(make_sample_buffer(200)));
    assert_eq!(cache.current_bytes(), 200);

    // Still retrievable
    assert!(cache.get(&IfdId(100)).is_some());
}

#[test]
fn synthetic_level_cache_default_budget_holds_common_tail_overview_level() {
    let cache = SyntheticLevelCache::new(DEFAULT_SYNTHETIC_LEVEL_CACHE_BYTES);
    let common_tail_level_bytes = 1674_u64 * 1100 * 3;

    assert!(
        cache.max_bytes() >= common_tail_level_bytes,
        "default synthetic cache should hold a common NDPI tail overview level"
    );
}

#[test]
fn tiff_private_cache_defaults_sum_to_thirty_two_mib() {
    let (full, strips, mcu, synthetic) =
        super::super::reader::private_cache_budgets(crate::CacheConfig::deterministic());

    assert_eq!(full + strips + mcu + synthetic * 2, 32 * 1024 * 1024);
}

#[test]
fn private_cache_environment_shares_are_clamped_to_the_aggregate() {
    let [full, strips, mcu, synthetic] =
        super::super::reader::clamp_private_cache_budgets(1_000, [10_000; 4]);

    assert!(full + strips + mcu + synthetic * 2 <= 1_000);
    assert!(full > 0 && strips > 0 && mcu > 0 && synthetic > 0);
}

#[test]
fn ndpi_mcu_starts_cache_evicts_by_retained_bytes() {
    let mut cache = NdpiMcuStartsCache::new(200);
    let first_key = (IfdId(1), 65426, 0, 100);
    let second_key = (IfdId(2), 65426, 100, 100);

    cache.put(first_key, Arc::new(vec![1; 8]));
    cache.put(second_key, Arc::new(vec![2; 8]));

    assert!(cache.current_bytes() <= cache.max_bytes());
    assert!(cache.get(&first_key).is_none());
    assert!(cache.get(&second_key).is_some());
}

#[test]
fn ndpi_relative_index_markers_share_the_existing_byte_bound_and_evict() {
    let relative = (IfdId(1), 65426, 8, 100);
    let normalized = (IfdId(2), 65426, 108, 100);
    for budget in [0, 1, 127, 128, 200] {
        let mut cache = NdpiMcuStartsCache::new(budget);
        cache.put_relative(relative);
        assert_eq!(cache.get(&relative).is_some(), budget >= 128);
        cache.put(normalized, Arc::new(vec![1, 2, 3]));
        assert!(cache.current_bytes() <= budget);
        assert!(cache.get(&relative).is_none(), "disabled or evicted marker");
        assert_eq!(cache.get(&normalized).is_some(), budget >= 88);
        cache.put_relative(relative);
        assert_eq!(cache.get(&relative).is_some(), budget >= 128);
        assert!(cache.current_bytes() <= budget);
    }
}
