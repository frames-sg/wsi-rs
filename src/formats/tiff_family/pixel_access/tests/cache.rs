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
