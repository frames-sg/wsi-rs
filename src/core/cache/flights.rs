//! Coalesce active region misses without retaining another decoded-tile cache.
use super::{CacheKey, CpuTile, TileCache};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::ThreadId;

pub(crate) struct TileFlights<K: Hash + Eq = CacheKey> {
    entries: Mutex<HashMap<K, Weak<TileFlight>>>,
    limit: usize,
}

impl<K: Hash + Eq> TileFlights<K> {
    pub(crate) fn new(cache_bytes: u64) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            // Only active operations own these records. Bound coordination even
            // for arbitrarily large request batches; disabled caches bypass it.
            limit: (cache_bytes / 128).min(128) as usize,
        }
    }
}

pub(crate) enum TileClaim<'a, K: Hash + Eq = CacheKey> {
    Ready(Arc<CpuTile>),
    Producer(TileProducer<'a, K>),
    Waiter(Arc<TileFlight>),
    Uncoalesced,
}

pub(crate) struct TileFlight {
    owner: ThreadId,
    result: Mutex<(bool, Option<Arc<CpuTile>>)>,
    ready: Condvar,
}

impl TileFlight {
    pub(crate) fn wait(&self) -> Option<Arc<CpuTile>> {
        let mut result = self.result.lock().unwrap_or_else(|e| e.into_inner());
        while !result.0 {
            result = self.ready.wait(result).unwrap_or_else(|e| e.into_inner());
        }
        result.1.clone()
    }
}

pub(crate) struct TileProducer<'a, K: Hash + Eq = CacheKey> {
    flights: &'a TileFlights<K>,
    key: K,
    flight: Arc<TileFlight>,
}

impl<K: Hash + Eq> TileProducer<'_, K> {
    pub(crate) fn complete(self, tile: Arc<CpuTile>) {
        self.flight
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1 = Some(tile);
    }
}

impl<K: Hash + Eq> Drop for TileProducer<'_, K> {
    fn drop(&mut self) {
        // Also release waiters when a read fails or unwinds. Failed reads are
        // retried by each caller so its original typed error is preserved.
        self.flight
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0 = true;
        self.flights
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
        self.flight.ready.notify_all();
    }
}

impl TileCache {
    pub(crate) fn claim_miss(&self, key: &CacheKey) -> TileClaim<'_> {
        self.flights.claim_miss(key, || {
            self.inner
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .lru
                .get(key)
                .cloned()
        })
    }
}

impl<K: Hash + Eq + Clone> TileFlights<K> {
    pub(crate) fn claim_miss(
        &self,
        key: &K,
        cached: impl FnOnce() -> Option<Arc<CpuTile>>,
    ) -> TileClaim<'_, K> {
        // Never block a decode-pool worker waiting for work queued to its own
        // pool. NDPI's existing source-strip coalescing remains independent.
        if self.limit == 0 || rayon::current_thread_index().is_some() {
            return TileClaim::Uncoalesced;
        }
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // The initial cache lookup may have raced a completed producer.
        if let Some(tile) = cached() {
            return TileClaim::Ready(tile);
        }
        let owner = std::thread::current().id();
        if let Some(flight) = entries.get(key).and_then(Weak::upgrade) {
            return if flight.owner == owner {
                TileClaim::Uncoalesced
            } else {
                TileClaim::Waiter(flight)
            };
        }
        if entries.len() >= self.limit {
            return TileClaim::Uncoalesced;
        }
        let flight = Arc::new(TileFlight {
            owner,
            result: Mutex::new((false, None)),
            ready: Condvar::new(),
        });
        entries.insert(key.clone(), Arc::downgrade(&flight));
        TileClaim::Producer(TileProducer {
            flights: self,
            key: key.clone(),
            flight,
        })
    }
}

#[cfg(test)]
mod tests;
