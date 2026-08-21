use super::super::*;
use super::support::{BatchCountingSource, CountingSource, MockSource};
use crate::test_support::region_request;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn shared_tile_cache_attachment_survives_owner_drop_and_replacement() {
    let first_reads = Arc::new(AtomicUsize::new(0));
    let second_reads = Arc::new(AtomicUsize::new(0));
    let first = Slide::from_source(
        Box::new(CountingSource::new(
            DatasetId::new(501),
            first_reads.clone(),
        )),
        Arc::new(TileCache::new(1024)),
    );
    let second = Slide::from_source(
        Box::new(CountingSource::new(
            DatasetId::new(501),
            second_reads.clone(),
        )),
        Arc::new(TileCache::new(1024)),
    );
    let shared = Arc::new(TileCache::new(256 * 256 * 3));
    first.replace_shared_tile_cache(shared.clone());
    second.replace_shared_tile_cache(shared.clone());
    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 256, 256);

    first.read_region(&req).expect("cold shared-cache read");
    assert_eq!(first_reads.load(Ordering::SeqCst), 1);
    second.read_region(&req).expect("warm cross-slide read");
    assert_eq!(second_reads.load(Ordering::SeqCst), 0);
    assert_eq!(shared.stats().hits, 1);
    drop(shared);
    second
        .read_region(&req)
        .expect("attached cache outlives released external owner");
    assert_eq!(second_reads.load(Ordering::SeqCst), 0);

    let replacement = Arc::new(TileCache::new(256 * 256 * 3));
    let detached = second.replace_shared_tile_cache(replacement.clone());
    assert_eq!(detached.stats().hits, 2);
    second
        .read_region(&req)
        .expect("replacement cache cold read");
    assert_eq!(second_reads.load(Ordering::SeqCst), 1);
    assert_eq!(replacement.stats().misses, 1);
}

#[test]
fn read_region_uses_cache() {
    let source: Box<dyn SlideReader> = Box::new(MockSource::new());
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(source, cache.clone());

    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 100, 100);

    // First read populates cache
    let _ = handle.read_region(&req).unwrap();

    // Verify tile is now cached
    let key = CacheKey {
        dataset_id: DatasetId::new(1),
        scene: 0,
        series: 0,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
        tile_col: 0,
        tile_row: 0,
    };
    assert!(cache.get(&key).is_some());

    // Second read should use cache (same result)
    let buf2 = handle.read_region(&req).unwrap();
    assert_eq!(buf2.data.as_u8().unwrap()[0], 255); // still red
}

#[test]
fn shared_cache_reuses_tile_across_handles() {
    let tile_reads = Arc::new(AtomicUsize::new(0));
    let shared_cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle_a = Slide::from_source(
        Box::new(CountingSource::new(DatasetId::new(7), tile_reads.clone())),
        shared_cache.clone(),
    );
    let handle_b = Slide::from_source(
        Box::new(CountingSource::new(DatasetId::new(7), tile_reads.clone())),
        shared_cache,
    );

    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 64, 64);

    let _ = handle_a.read_region(&req).unwrap();
    assert_eq!(tile_reads.load(Ordering::SeqCst), 1);

    let _ = handle_b.read_region(&req).unwrap();
    assert_eq!(
        tile_reads.load(Ordering::SeqCst),
        1,
        "second handle should reuse the shared cached tile"
    );
}

#[test]
fn read_region_batches_uncached_tiles_and_preserves_cache() {
    let tile_reads = Arc::new(AtomicUsize::new(0));
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let batch_tile_count = Arc::new(AtomicUsize::new(0));
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(
        Box::new(BatchCountingSource::new(
            tile_reads.clone(),
            batch_reads.clone(),
            batch_tile_count.clone(),
        )),
        cache,
    );

    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 512, 256);

    let first = handle.read_region(&req).unwrap();
    let pixels = first.data.as_u8().unwrap();
    assert_eq!(&pixels[..3], &[255, 0, 0]);
    assert_eq!(&pixels[(256 * 3)..(257 * 3)], &[0, 255, 0]);
    assert_eq!(tile_reads.load(Ordering::SeqCst), 0);
    assert_eq!(batch_reads.load(Ordering::SeqCst), 1);
    assert_eq!(batch_tile_count.load(Ordering::SeqCst), 2);

    let second = handle.read_region(&req).unwrap();
    assert_eq!(second.data.as_u8().unwrap(), pixels);
    assert_eq!(tile_reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        batch_reads.load(Ordering::SeqCst),
        1,
        "second read should be fully satisfied from cache"
    );
}

#[test]
fn read_region_batch_cache_behavior_is_observable_in_internal_stats() {
    let tile_reads = Arc::new(AtomicUsize::new(0));
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let batch_tile_count = Arc::new(AtomicUsize::new(0));
    let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
    let handle = Slide::from_source(
        Box::new(BatchCountingSource::new(
            tile_reads,
            batch_reads.clone(),
            batch_tile_count,
        )),
        cache.clone(),
    );

    let req = region_request(0, 0, 0, PlaneSelection::default(), 0, 0, 512, 256);

    let before = cache.stats();
    assert_eq!(before.hits, 0);
    assert_eq!(before.misses, 0);

    let _ = handle.read_region(&req).unwrap();
    let cold = cache.stats();
    assert_eq!(batch_reads.load(Ordering::SeqCst), 1);
    assert_eq!(cold.hits, 0);
    assert_eq!(cold.misses, 2);
    assert_eq!(cold.puts, 2);

    let _ = handle.read_region(&req).unwrap();
    let warm = cache.stats();
    assert_eq!(batch_reads.load(Ordering::SeqCst), 1);
    assert_eq!(warm.hits, 2);
    assert_eq!(warm.misses, 2);
    assert_eq!(warm.puts, 2);
}

#[test]
fn display_tile_exact_regular_reads_use_display_cache() {
    let tile_reads = Arc::new(AtomicUsize::new(0));
    let handle = Slide::from_source(
        Box::new(CountingSource::new(DatasetId::new(8), tile_reads.clone())),
        Arc::new(TileCache::new(64 * 1024 * 1024)),
    );

    let req = TileViewRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
        tile_width: 256,
        tile_height: 256,
    };

    let _ = handle.read_display_tile(&req).unwrap();
    assert_eq!(tile_reads.load(Ordering::SeqCst), 1);

    let _ = handle.read_display_tile(&req).unwrap();
    assert_eq!(
        tile_reads.load(Ordering::SeqCst),
        1,
        "second exact display-tile read should hit the display cache"
    );
}
