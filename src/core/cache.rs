use lru::LruCache;
use std::sync::{Arc, Mutex};

use crate::core::types::{CpuTile, DatasetId};

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
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self::deterministic()
    }
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
/// Note: scene/series are u32 here (not usize) to keep CacheKey compact and
/// Hash-friendly. TileRequest/RegionRequest use usize for ergonomic indexing.
/// Slide converts usize → u32 via `as u32` when constructing cache keys.
/// Overflow is not a practical concern (>4B scenes/series is impossible).
pub struct CacheKey {
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

pub struct TileCache {
    inner: Mutex<TileCacheState>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CacheStats {
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) puts: u64,
    pub(crate) evictions: u64,
    pub(crate) rejected_oversize: u64,
    pub(crate) capacity_bytes: u64,
    pub(crate) current_bytes: u64,
    pub(crate) entries: usize,
}

impl std::fmt::Debug for TileCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("TileCache")
            .field("capacity_bytes", &state.capacity_bytes)
            .field("current_bytes", &state.current_bytes)
            .field("entries", &state.lru.len())
            .field("hits", &state.hits)
            .field("misses", &state.misses)
            .finish()
    }
}

struct TileCacheState {
    lru: LruCache<CacheKey, CachedTile>,
    capacity_bytes: u64,
    current_bytes: u64,
    hits: u64,
    misses: u64,
    puts: u64,
    evictions: u64,
    rejected_oversize: u64,
}

struct CachedTile {
    data: Arc<CpuTile>,
    byte_size: u64,
}

impl TileCache {
    pub(crate) fn new(capacity_bytes: u64) -> Self {
        Self {
            inner: Mutex::new(TileCacheState {
                // The cache is byte-budgeted only. The backing LRU stays unbounded
                // and eviction is driven by `capacity_bytes`.
                lru: LruCache::unbounded(),
                capacity_bytes,
                current_bytes: 0,
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

        if byte_size > state.capacity_bytes {
            state.rejected_oversize += 1;
            return;
        }

        // Remove existing entry if present
        if let Some((_, existing)) = state.lru.pop_entry(&key) {
            state.current_bytes -= existing.byte_size;
        }

        // Evict LRU entries until there's room
        while state.current_bytes + byte_size > state.capacity_bytes {
            if let Some((_, evicted)) = state.lru.pop_lru() {
                state.current_bytes -= evicted.byte_size;
                state.evictions += 1;
            } else {
                break;
            }
        }

        state.lru.put(key, CachedTile { data, byte_size });
        state.current_bytes += byte_size;
        state.puts += 1;
    }

    pub(crate) fn get(&self, key: &CacheKey) -> Option<Arc<CpuTile>> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cached = state.lru.get(key).map(|entry| entry.data.clone());
        if cached.is_some() {
            state.hits += 1;
        } else {
            state.misses += 1;
        }
        cached
    }

    pub(crate) fn stats(&self) -> CacheStats {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        CacheStats {
            hits: state.hits,
            misses: state.misses,
            puts: state.puts,
            evictions: state.evictions,
            rejected_oversize: state.rejected_oversize,
            capacity_bytes: state.capacity_bytes,
            current_bytes: state.current_bytes,
            entries: state.lru.len(),
        }
    }

    pub(crate) fn display_default() -> Self {
        Self::new(capacity_from_env(
            DISPLAY_TILE_CACHE_BYTES_ENV,
            DEFAULT_DISPLAY_TILE_CACHE_SIZE,
        ))
    }

    pub(crate) fn display_with_config(config: CacheConfig) -> Self {
        Self::new(config.display_tile_budget())
    }

    pub(crate) fn shared_default_with_hint(default_bytes: u64) -> Self {
        Self::new(capacity_from_env(TILE_CACHE_BYTES_ENV, default_bytes))
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

fn capacity_from_env(env_name: &str, default_bytes: u64) -> u64 {
    std::env::var(env_name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|bytes| *bytes > 0)
        .unwrap_or(default_bytes)
}

#[cfg(test)]
mod tile_cache_tests {
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
        // Both fit: 200 bytes
        assert!(cache.get(&make_key(1, 0, 0, 0)).is_some());
        assert!(cache.get(&make_key(1, 0, 1, 0)).is_some());

        // Third pushes over 250
        cache.put(make_key(1, 0, 2, 0), Arc::new(make_sample_buffer(100)));
        assert!(cache.get(&make_key(1, 0, 0, 0)).is_none()); // evicted
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
}
