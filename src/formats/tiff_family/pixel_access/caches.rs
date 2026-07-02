use super::*;
use std::hash::Hash;

// ── FullDecodeCache ───────────────────────────────────────────────

/// Default maximum cache size: 128 MB.
pub(super) const DEFAULT_FULL_DECODE_CACHE_BYTES: u64 = 128 * 1024 * 1024;
pub(super) const FULL_DECODE_CACHE_BYTES_ENV: &str = "WSI_RS_FULL_DECODE_CACHE_BYTES";
/// Default maximum cache size for decoded NDPI strips: 1 MB.
///
/// Large NDPI display traces are often one-way walks through strip space. Keep
/// the default tight for predictable RSS and use `WSI_RS_NDPI_STRIP_CACHE_BYTES`
/// for repeated-region workloads that benefit from a larger working set.
pub(super) const DEFAULT_NDPI_STRIP_CACHE_BYTES: u64 = 1024 * 1024;
pub(super) const NDPI_STRIP_CACHE_BYTES_ENV: &str = "WSI_RS_NDPI_STRIP_CACHE_BYTES";
/// Default maximum cache size for synthetic NDPI tail levels: 16 MB.
pub(super) const DEFAULT_SYNTHETIC_LEVEL_CACHE_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const SYNTHETIC_LEVEL_CACHE_BYTES_ENV: &str = "WSI_RS_SYNTHETIC_LEVEL_CACHE_BYTES";
pub(super) const DEFAULT_JP2K_SHARED_TILE_CACHE_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const DEFAULT_STITCHED_COMPONENT_TILE_CACHE_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const NDPI_DISPLAY_WIDE_STRIP_BATCH: usize = 4;
pub(super) const NDPI_DISPLAY_NARROW_STRIP_BATCH: usize = 8;
#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) const JPEG_DEVICE_DECODE_ENV: &str = "WSI_RS_JPEG_DEVICE_DECODE";
#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) const JP2K_DEVICE_DECODE_ENV: &str = "WSI_RS_JP2K_DEVICE_DECODE";

pub(super) type NdpiMcuStartsCache = HashMap<(IfdId, u16), Arc<Vec<u64>>>;
pub(super) type SyntheticDeepestKey = (usize, usize, u32, u32, u32);
pub(super) type SyntheticDeepestValue = (u32, u32, u32);
pub(super) const NDPI_DISPLAY_WIDE_STRIP_WIDTH: u32 = 1024;

pub(super) struct NdpiJpegTilePayload {
    pub(super) jpeg: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn jpeg_device_decode_enabled() -> bool {
    std::env::var(JPEG_DEVICE_DECODE_ENV).is_ok_and(|value| {
        value.eq_ignore_ascii_case("1")
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("yes")
            || value.eq_ignore_ascii_case("on")
    })
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn jp2k_device_decode_enabled() -> bool {
    std::env::var(JP2K_DEVICE_DECODE_ENV).is_ok_and(|value| {
        value.eq_ignore_ascii_case("1")
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("yes")
            || value.eq_ignore_ascii_case("on")
    })
}

pub(super) struct ByteSizedTileCache<K, const DEFAULT_BYTES: u64> {
    pub(super) entries: LruCache<K, Arc<CpuTile>>,
    pub(super) current_bytes: u64,
    pub(super) max_bytes: u64,
}

impl<K, const DEFAULT_BYTES: u64> ByteSizedTileCache<K, DEFAULT_BYTES>
where
    K: Eq + Hash,
{
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            entries: LruCache::unbounded(),
            current_bytes: 0,
            max_bytes,
        }
    }
}

impl<K, const DEFAULT_BYTES: u64> ByteSizedTileCache<K, DEFAULT_BYTES>
where
    K: Eq + Hash,
{
    pub(super) fn get(&mut self, key: &K) -> Option<Arc<CpuTile>> {
        self.entries.get(key).cloned()
    }

    pub(super) fn put(&mut self, key: K, data: Arc<CpuTile>) {
        let byte_size = data.data.byte_size() as u64;

        if byte_size > self.max_bytes {
            return;
        }

        if let Some((_, existing)) = self.entries.pop_entry(&key) {
            self.current_bytes -= existing.data.byte_size() as u64;
        }

        while self.current_bytes + byte_size > self.max_bytes {
            if let Some((_, evicted)) = self.entries.pop_lru() {
                self.current_bytes -= evicted.data.byte_size() as u64;
            } else {
                break;
            }
        }

        self.entries.put(key, data);
        self.current_bytes += byte_size;
    }
}

impl<K, const DEFAULT_BYTES: u64> Default for ByteSizedTileCache<K, DEFAULT_BYTES>
where
    K: Eq + Hash,
{
    fn default() -> Self {
        Self::new(DEFAULT_BYTES)
    }
}

/// Byte-budgeted LRU cache for fully decoded NDPI levels.
///
/// NDPI levels without restart markers require decoding the entire JPEG
/// image to extract a single tile. This cache stores the decoded image
/// so subsequent tile requests from the same level are satisfied from
/// memory instead of re-decoding.
pub(super) type FullDecodeCache = ByteSizedTileCache<IfdId, DEFAULT_FULL_DECODE_CACHE_BYTES>;
pub(super) type NdpiStripCache = ByteSizedTileCache<NdpiStripKey, DEFAULT_NDPI_STRIP_CACHE_BYTES>;
pub(super) type SyntheticLevelCache =
    ByteSizedTileCache<SyntheticLevelKey, DEFAULT_SYNTHETIC_LEVEL_CACHE_BYTES>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct StitchedComponentTileKey {
    pub(super) ifd_id: IfdId,
    pub(super) tile_idx: usize,
    pub(super) width: u32,
    pub(super) height: u32,
}

pub(super) type StitchedComponentTileCache =
    ByteSizedTileCache<StitchedComponentTileKey, DEFAULT_STITCHED_COMPONENT_TILE_CACHE_BYTES>;

#[derive(Clone, Debug, Default)]
pub(super) struct FullDecodeFlight {
    pub(super) waiters: usize,
    pub(super) result: Option<Result<Arc<CpuTile>, String>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct NdpiStripKey {
    pub(super) ifd_id: IfdId,
    pub(super) col: u32,
    pub(super) native_row: u32,
}

#[derive(Clone, Debug, Default)]
pub(super) struct NdpiStripFlight {
    pub(super) waiters: usize,
    pub(super) result: Option<Result<Arc<CpuTile>, String>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct SyntheticLevelKey {
    pub(super) scene: usize,
    pub(super) series: usize,
    pub(super) base_level: u32,
    pub(super) target_level: u32,
    pub(super) z: u32,
    pub(super) c: u32,
    pub(super) t: u32,
}

#[derive(Clone, Debug, Default)]
pub(super) struct SyntheticLevelFlight {
    pub(super) waiters: usize,
    pub(super) result: Option<Result<Arc<CpuTile>, String>>,
}
