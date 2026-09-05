use super::*;
use crate::{ColorSpace, CpuTile, DatasetId, TileRequest};

fn key(col: i64) -> CacheKey {
    CacheKey::from_tile_request(
        DatasetId::new(1),
        &TileRequest::new(0usize, 0usize, 0u32, col, 0),
    )
}

fn producer<'a>(cache: &'a TileCache, key: &CacheKey) -> TileProducer<'a> {
    match cache.claim_miss(key) {
        TileClaim::Producer(p) => p,
        _ => panic!("expected ownership of the cold tile"),
    }
}

#[test]
fn flight_records_are_bounded_and_disabled_with_tiny_caches() {
    for budget in [0, 1, 127] {
        let cache = TileCache::new(budget);
        assert!(matches!(cache.claim_miss(&key(0)), TileClaim::Uncoalesced));
        assert_eq!(cache.stats().capacity_bytes, budget);
    }
    for (budget, limit) in [(512, 4), (64 * 1024 * 1024, 128)] {
        let cache = TileCache::new(budget);
        let claims: Vec<_> = (0..limit).map(|i| producer(&cache, &key(i))).collect();
        assert!(matches!(
            cache.claim_miss(&key(limit)),
            TileClaim::Uncoalesced
        ));
        assert_eq!(cache.flights.entries.lock().unwrap().len(), limit as usize);
        drop(claims);
        assert!(cache.flights.entries.lock().unwrap().is_empty());
        drop(producer(&cache, &key(limit)));
    }
}

#[test]
fn failed_producer_releases_waiters_and_allows_retry() {
    let cache = TileCache::new(1024);
    let p = producer(&cache, &key(0));
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        let cache = &cache;
        let waiter = scope.spawn(move || {
            let TileClaim::Waiter(flight) = cache.claim_miss(&key(0)) else {
                panic!("expected waiter")
            };
            tx.send(()).unwrap();
            flight.wait()
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        drop(p);
        assert!(waiter.join().unwrap().is_none());
    });
    drop(producer(&cache, &key(0)));
    assert!(cache.flights.entries.lock().unwrap().is_empty());
}

#[test]
fn completed_pixels_survive_eviction_only_for_active_waiters() {
    let cache = TileCache::new(256);
    let p = producer(&cache, &key(0));
    let tile =
        Arc::new(CpuTile::from_u8_interleaved(64, 1, 3, ColorSpace::Rgb, vec![17; 192]).unwrap());
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let cache = &cache;
        let waiter = scope.spawn(move || {
            let TileClaim::Waiter(flight) = cache.claim_miss(&key(0)) else {
                panic!("expected waiter")
            };
            tx.send(()).unwrap();
            held.recv().unwrap();
            flight.wait().unwrap()
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        cache.put(key(0), tile.clone());
        p.complete(tile.clone());
        cache.put(key(1), tile.clone());
        assert!(cache.get(&key(0)).is_none());
        release.send(()).unwrap();
        assert!(Arc::ptr_eq(&tile, &waiter.join().unwrap()));
    });
    assert!(cache.flights.entries.lock().unwrap().is_empty());
    assert!(cache.stats().current_bytes <= 256);
}

#[test]
fn recursion_pool_workers_and_unwinding_cannot_strand_a_flight() {
    let cache = TileCache::new(1024);
    let p = producer(&cache, &key(0));
    assert!(matches!(cache.claim_miss(&key(0)), TileClaim::Uncoalesced));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    pool.install(|| assert!(matches!(cache.claim_miss(&key(0)), TileClaim::Uncoalesced)));
    drop(p);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _p = producer(&cache, &key(0));
        panic!("source panic");
    }));
    assert!(panic.is_err());
    drop(producer(&cache, &key(0)));
}
