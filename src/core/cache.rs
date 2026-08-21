use lru::LruCache;
use std::borrow::Borrow;
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use crate::core::environment;
use crate::core::types::{CpuTile, DatasetId, RegionRequest, TileRequest};

// ── TileCache (axis-aware) ────────────────────────────────────────

/// Default shared decoded tile cache.
///
/// Standard Aperio SVS JPEG tiles are commonly 240x240 RGB, or about 170 KiB
/// per decoded tile. A 64 MiB budget keeps a few hundred such source tiles
/// resident, which is enough for normal viewport overlap during quick zooms
/// without forcing users to tune cache options before the viewer is usable.
pub(crate) const DEFAULT_TILE_CACHE_SIZE: u64 = 64 * 1024 * 1024;
const TILE_CACHE_BYTES_ENV: &str = "WSI_RS_TILE_CACHE_BYTES";
/// Default display-tile cache.
///
/// Display-tile reads on regular tiled slides cache the decoded source tiles
/// used for composition. Keep enough room for at least a dense viewport plus
/// adjacent zoom/pan overlap; 1 MiB only held a handful of SVS tiles and caused
/// immediate churn during zoom-out bursts.
pub(crate) const DEFAULT_DISPLAY_TILE_CACHE_SIZE: u64 = 32 * 1024 * 1024;
const DISPLAY_TILE_CACHE_BYTES_ENV: &str = "WSI_RS_DISPLAY_TILE_CACHE_BYTES";
// Count-bounded private LRUs still allocate per-entry bookkeeping. Include a
// conservative floor so tiny source tiles cannot turn a byte budget into an
// enormous hash-table capacity at open time.
const PRIVATE_CACHE_ENTRY_ACCOUNTING_FLOOR_BYTES: u64 = 256;
// Private per-format caches supplement the public shared tile cache. Bound
// their aggregate entry capacity to one quarter of the configured shared
// cache so opening a slide cannot multiply the caller's byte policy.
const PRIVATE_CACHE_BUDGET_DIVISOR: u64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CacheConfig {
    pub shared_tile_bytes: Option<u64>,
    pub display_tile_bytes: Option<u64>,
}

impl CacheConfig {
    pub const fn deterministic() -> Self {
        Self {
            shared_tile_bytes: None,
            display_tile_bytes: None,
        }
    }

    pub const fn with_shared_tile_bytes(mut self, bytes: u64) -> Self {
        self.shared_tile_bytes = Some(bytes);
        self
    }

    pub const fn with_display_tile_bytes(mut self, bytes: u64) -> Self {
        self.display_tile_bytes = Some(bytes);
        self
    }

    pub(crate) fn shared_tile_budget(self, source_hint: Option<u64>) -> u64 {
        self.shared_tile_bytes
            .or(source_hint)
            .unwrap_or(DEFAULT_TILE_CACHE_SIZE)
    }

    pub(crate) fn display_tile_budget(self) -> u64 {
        self.display_tile_bytes
            .unwrap_or(DEFAULT_DISPLAY_TILE_CACHE_SIZE)
    }

    pub(crate) fn private_cache_budget(self, cache_count: usize) -> PrivateCacheBudget {
        PrivateCacheBudget {
            remaining_bytes: self.private_cache_budget_bytes(),
            remaining_caches: cache_count,
        }
    }

    pub(crate) fn private_cache_budget_bytes(self) -> u64 {
        self.shared_tile_budget(None) / PRIVATE_CACHE_BUDGET_DIVISOR
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self::deterministic()
    }
}

/// One slide's aggregate budget for count-bounded format-private caches.
///
/// Allocations are made against estimated retained bytes (including a floor
/// for LRU bookkeeping). A cache receives zero entries when the remaining
/// budget cannot account for one entry, avoiding eager hash-table allocation.
pub(crate) struct PrivateCacheBudget {
    remaining_bytes: u64,
    remaining_caches: usize,
}

impl PrivateCacheBudget {
    pub(crate) fn allocate(&mut self, estimated_entry_bytes: u64) -> PrivateCacheCapacity {
        if self.remaining_caches == 0 {
            return PrivateCacheCapacity::default();
        }

        let cache_count = self.remaining_caches as u64;
        self.remaining_caches -= 1;
        let accounted_entry_bytes =
            estimated_entry_bytes.max(PRIVATE_CACHE_ENTRY_ACCOUNTING_FLOOR_BYTES);
        let fair_share = self.remaining_bytes / cache_count;
        let mut entries = fair_share / accounted_entry_bytes;
        if entries == 0 && self.remaining_bytes >= accounted_entry_bytes {
            entries = 1;
        }
        entries = entries.min(usize::MAX as u64);
        let accounted_bytes = entries
            .checked_mul(accounted_entry_bytes)
            .unwrap_or(self.remaining_bytes)
            .min(self.remaining_bytes);
        self.remaining_bytes -= accounted_bytes;

        PrivateCacheCapacity {
            entries: entries as usize,
            accounted_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PrivateCacheCapacity {
    entries: usize,
    accounted_bytes: u64,
}

/// A count-bounded private LRU that can be completely disabled.
///
/// `lru::LruCache::new` preallocates for its entry capacity and requires a
/// non-zero value. Keeping the disabled state as `None` makes a zero-budget
/// cache allocation-free and causes inserts to be ignored.
#[derive(Debug)]
pub(crate) struct PrivateCache<K: Hash + Eq, V> {
    lru: Option<LruCache<K, V>>,
    capacity: PrivateCacheCapacity,
}

impl<K: Hash + Eq, V> PrivateCache<K, V> {
    pub(crate) fn new(capacity: PrivateCacheCapacity) -> Self {
        let lru = NonZeroUsize::new(capacity.entries).map(LruCache::new);
        Self { lru, capacity }
    }

    pub(crate) fn get<'a, Q>(&'a mut self, key: &Q) -> Option<&'a V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.lru.as_mut()?.get(key)
    }

    pub(crate) fn put(&mut self, key: K, value: V) {
        if let Some(lru) = &mut self.lru {
            lru.put(key, value);
        }
    }

    pub(crate) fn capacity_entries(&self) -> usize {
        self.capacity.entries
    }

    #[cfg(test)]
    pub(crate) fn accounted_capacity_bytes(&self) -> u64 {
        self.capacity.accounted_bytes
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lru.as_ref().map_or(0, LruCache::len)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WeightedLruPut {
    Inserted { evictions: u64 },
    RejectedOversize,
}

struct WeightedEntry<V> {
    value: V,
    bytes: u64,
}

/// An LRU whose capacity is the retained byte weight, not its entry count.
///
/// Callers own synchronization and statistics. Replacing a key does not count
/// as an eviction, and an oversized replacement leaves the existing value
/// untouched.
pub(crate) struct WeightedLru<K: Hash + Eq, V> {
    entries: LruCache<K, WeightedEntry<V>>,
    capacity_bytes: u64,
    current_bytes: u64,
}

impl<K: Hash + Eq, V> WeightedLru<K, V> {
    pub(crate) fn new(capacity_bytes: u64) -> Self {
        Self {
            entries: LruCache::unbounded(),
            capacity_bytes,
            current_bytes: 0,
        }
    }

    pub(crate) fn get<'a, Q>(&'a mut self, key: &Q) -> Option<&'a V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entries.get(key).map(|entry| &entry.value)
    }

    pub(crate) fn put(&mut self, key: K, value: V, bytes: u64) -> WeightedLruPut {
        if bytes > self.capacity_bytes {
            return WeightedLruPut::RejectedOversize;
        }

        if let Some((_, existing)) = self.entries.pop_entry(&key) {
            self.current_bytes -= existing.bytes;
        }

        let mut evictions = 0;
        while self.current_bytes > self.capacity_bytes - bytes {
            let Some((_, evicted)) = self.entries.pop_lru() else {
                break;
            };
            self.current_bytes -= evicted.bytes;
            evictions += 1;
        }

        self.entries.put(key, WeightedEntry { value, bytes });
        self.current_bytes += bytes;
        WeightedLruPut::Inserted { evictions }
    }

    pub(crate) fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes
    }

    pub(crate) fn current_bytes(&self) -> u64 {
        self.current_bytes
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
/// Note: scene/series are u32 here (not usize) to keep CacheKey compact and
/// Hash-friendly. TileRequest/RegionRequest use usize for ergonomic indexing.
/// Slide converts usize → u32 via `as u32` when constructing cache keys.
/// Overflow is not a practical concern (>4B scenes/series is impossible).
pub(crate) struct CacheKey {
    pub(crate) dataset_id: DatasetId,
    pub(crate) scene: u32,
    pub(crate) series: u32,
    pub(crate) level: u32,
    pub(crate) z: u32,
    pub(crate) c: u32,
    pub(crate) t: u32,
    pub(crate) tile_col: i64,
    pub(crate) tile_row: i64,
}

impl CacheKey {
    pub(crate) fn from_tile_request(dataset_id: DatasetId, request: &TileRequest) -> Self {
        let plane = request.plane.get();
        Self {
            dataset_id,
            scene: request.scene.get() as u32,
            series: request.series.get() as u32,
            level: request.level.get(),
            z: plane.z,
            c: plane.c,
            t: plane.t,
            tile_col: request.col,
            tile_row: request.row,
        }
    }

    pub(crate) fn from_region_tile(
        dataset_id: DatasetId,
        request: &RegionRequest,
        col: i64,
        row: i64,
    ) -> Self {
        Self::from_tile_request(
            dataset_id,
            &TileRequest {
                scene: request.scene,
                series: request.series,
                level: request.level,
                plane: request.plane,
                col,
                row,
            },
        )
    }
}

/// Thread-safe, byte-bounded decoded tile cache that can be shared by slides.
pub struct TileCache {
    inner: Mutex<TileCacheState>,
}

/// Snapshot of byte-sized decoded tile cache activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TileCacheStats {
    /// Successful lookups.
    pub hits: u64,
    /// Unsuccessful lookups.
    pub misses: u64,
    /// Entries admitted to the cache.
    pub puts: u64,
    /// Entries removed to remain within the byte capacity.
    pub evictions: u64,
    /// Entries rejected because one value exceeded the whole capacity.
    pub rejected_oversize: u64,
    /// Configured byte capacity.
    pub capacity_bytes: u64,
    /// Bytes currently retained by cached entries.
    pub current_bytes: u64,
    /// Number of entries currently retained.
    pub entries: usize,
}

impl std::fmt::Debug for TileCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("TileCache")
            .field("capacity_bytes", &state.lru.capacity_bytes())
            .field("current_bytes", &state.lru.current_bytes())
            .field("entries", &state.lru.len())
            .field("hits", &state.hits)
            .field("misses", &state.misses)
            .finish()
    }
}

struct TileCacheState {
    lru: WeightedLru<CacheKey, Arc<CpuTile>>,
    hits: u64,
    misses: u64,
    puts: u64,
    evictions: u64,
    rejected_oversize: u64,
}

impl TileCache {
    /// Create a thread-safe decoded tile cache with a byte capacity.
    pub fn new(capacity_bytes: u64) -> Self {
        Self {
            inner: Mutex::new(TileCacheState {
                lru: WeightedLru::new(capacity_bytes),
                hits: 0,
                misses: 0,
                puts: 0,
                evictions: 0,
                rejected_oversize: 0,
            }),
        }
    }

    pub(crate) fn put(&self, key: CacheKey, data: Arc<CpuTile>) {
        let byte_size = data.data.byte_size() as u64;
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match state.lru.put(key, data, byte_size) {
            WeightedLruPut::Inserted { evictions } => {
                state.evictions += evictions;
                state.puts += 1;
            }
            WeightedLruPut::RejectedOversize => state.rejected_oversize += 1,
        }
    }

    pub(crate) fn get(&self, key: &CacheKey) -> Option<Arc<CpuTile>> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cached = state.lru.get(key).cloned();
        if cached.is_some() {
            state.hits += 1;
        } else {
            state.misses += 1;
        }
        cached
    }

    /// Return an atomic snapshot of cache capacity and activity counters.
    pub fn stats(&self) -> TileCacheStats {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        TileCacheStats {
            hits: state.hits,
            misses: state.misses,
            puts: state.puts,
            evictions: state.evictions,
            rejected_oversize: state.rejected_oversize,
            capacity_bytes: state.lru.capacity_bytes(),
            current_bytes: state.lru.current_bytes(),
            entries: state.lru.len(),
        }
    }

    pub(crate) fn display_default() -> Self {
        Self::new(environment::positive_u64(
            DISPLAY_TILE_CACHE_BYTES_ENV,
            DEFAULT_DISPLAY_TILE_CACHE_SIZE,
        ))
    }

    pub(crate) fn display_with_config(config: CacheConfig) -> Self {
        Self::new(config.display_tile_budget())
    }

    pub(crate) fn shared_default_with_hint(default_bytes: u64) -> Self {
        Self::new(environment::positive_u64(
            TILE_CACHE_BYTES_ENV,
            default_bytes,
        ))
    }

    pub(crate) fn shared_with_config(config: CacheConfig, source_hint: Option<u64>) -> Self {
        Self::new(config.shared_tile_budget(source_hint))
    }
}

impl Default for TileCache {
    fn default() -> Self {
        Self::shared_default_with_hint(DEFAULT_TILE_CACHE_SIZE)
    }
}

#[cfg(test)]
#[path = "cache/tests/tile_cache.rs"]
mod tile_cache_tests;
