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
pub(super) const NDPI_DISPLAY_WIDE_STRIP_BATCH: usize = 4;
pub(super) const NDPI_DISPLAY_NARROW_STRIP_BATCH: usize = 8;
#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) const JPEG_DEVICE_DECODE_ENV: &str = "WSI_RS_JPEG_DEVICE_DECODE";
#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) const JP2K_DEVICE_DECODE_ENV: &str = "WSI_RS_JP2K_DEVICE_DECODE";

pub(super) type NdpiMcuStartsCache = HashMap<(IfdId, u16, u64, u64), Arc<Vec<u64>>>;
pub(super) const NDPI_DISPLAY_WIDE_STRIP_WIDTH: u32 = 1024;

pub(super) struct NdpiJpegTilePayload {
    pub(super) jpeg: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn jpeg_device_decode_enabled() -> bool {
    crate::core::environment::flag(JPEG_DEVICE_DECODE_ENV)
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn jp2k_device_decode_enabled() -> bool {
    crate::core::environment::flag(JP2K_DEVICE_DECODE_ENV)
}

pub(super) struct ByteSizedTileCache<K: Eq + Hash> {
    entries: WeightedLru<K, Arc<CpuTile>>,
}

impl<K> ByteSizedTileCache<K>
where
    K: Eq + Hash,
{
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            entries: WeightedLru::new(max_bytes),
        }
    }
}

impl<K> ByteSizedTileCache<K>
where
    K: Eq + Hash,
{
    pub(super) fn get(&mut self, key: &K) -> Option<Arc<CpuTile>> {
        self.entries.get(key).cloned()
    }

    pub(super) fn put(&mut self, key: K, data: Arc<CpuTile>) {
        let byte_size = data.data.byte_size() as u64;
        self.entries.put(key, data, byte_size);
    }

    #[cfg(test)]
    pub(super) fn current_bytes(&self) -> u64 {
        self.entries.current_bytes()
    }

    pub(super) fn max_bytes(&self) -> u64 {
        self.entries.capacity_bytes()
    }
}

#[derive(Clone, Debug, Default)]
struct DecodeFlight {
    waiters: usize,
    result: Option<Result<Arc<CpuTile>, String>>,
}

/// A byte-bounded tile cache that coalesces concurrent misses for one key.
///
/// The loader runs without either mutex held. Successful values are retained
/// according to the byte budget; failures are delivered to current waiters but
/// are not cached, so a later request can retry.
pub(super) struct SingleFlightTileCache<K>
where
    K: Eq + Hash,
{
    cache: Mutex<ByteSizedTileCache<K>>,
    flights: Mutex<HashMap<K, DecodeFlight>>,
    ready: Condvar,
}

impl<K> SingleFlightTileCache<K>
where
    K: Clone + Eq + Hash,
{
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            cache: Mutex::new(ByteSizedTileCache::new(max_bytes)),
            flights: Mutex::new(HashMap::new()),
            ready: Condvar::new(),
        }
    }

    pub(super) fn get(&self, key: &K) -> Option<Arc<CpuTile>> {
        self.cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(key)
    }

    pub(super) fn put(&self, key: K, value: Arc<CpuTile>) {
        self.cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .put(key, value);
    }

    #[cfg(test)]
    pub(super) fn current_bytes(&self) -> u64 {
        self.cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .current_bytes()
    }

    pub(super) fn max_bytes(&self) -> u64 {
        self.cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .max_bytes()
    }

    pub(super) fn get_or_try_insert_with<F>(&self, key: K, load: F) -> Result<Arc<CpuTile>, String>
    where
        F: FnOnce() -> Result<Arc<CpuTile>, String>,
    {
        self.get_or_try_insert_with_error(key, load, std::convert::identity)
    }

    pub(super) fn get_or_try_insert_with_error<E, F, M>(
        &self,
        key: K,
        load: F,
        map_shared_error: M,
    ) -> Result<Arc<CpuTile>, E>
    where
        E: ToString,
        F: FnOnce() -> Result<Arc<CpuTile>, E>,
        M: Fn(String) -> E,
    {
        if let Some(value) = self.get(&key) {
            return Ok(value);
        }

        let mut flights = self.flights.lock().unwrap_or_else(|err| err.into_inner());
        let mut registered_waiter = false;
        loop {
            match flights.get_mut(&key) {
                Some(flight) => {
                    if !registered_waiter {
                        flight.waiters += 1;
                        registered_waiter = true;
                    }
                    if let Some(result) = flight.result.clone() {
                        flight.waiters -= 1;
                        if flight.waiters == 0 {
                            flights.remove(&key);
                        }
                        return result.map_err(&map_shared_error);
                    }
                    flights = self
                        .ready
                        .wait(flights)
                        .unwrap_or_else(|err| err.into_inner());
                }
                None if registered_waiter => {
                    return Err(map_shared_error(
                        "single-flight producer ended without a result".into(),
                    ));
                }
                None => {
                    // A producer may have populated the cache after our first
                    // lookup but before we acquired the flight lock.
                    if let Some(value) = self.get(&key) {
                        return Ok(value);
                    }
                    flights.insert(key.clone(), DecodeFlight::default());
                    break;
                }
            }
        }
        drop(flights);

        let mut cleanup = FlightCleanup {
            owner: self,
            key: Some(key.clone()),
        };
        let result = load();
        if let Ok(value) = &result {
            self.put(key.clone(), value.clone());
        }
        let shared_result = result.as_ref().map(Arc::clone).map_err(ToString::to_string);

        let mut flights = self.flights.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(flight) = flights.get_mut(&key) {
            flight.result = Some(shared_result);
            if flight.waiters == 0 {
                flights.remove(&key);
            }
        }
        cleanup.key = None;
        drop(flights);
        self.ready.notify_all();

        result
    }
}

struct FlightCleanup<'a, K>
where
    K: Eq + Hash,
{
    owner: &'a SingleFlightTileCache<K>,
    key: Option<K>,
}

impl<K> Drop for FlightCleanup<'_, K>
where
    K: Eq + Hash,
{
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        self.owner
            .flights
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&key);
        self.owner.ready.notify_all();
    }
}

/// Byte-budgeted LRU cache for fully decoded NDPI levels.
///
/// NDPI levels without restart markers require decoding the entire JPEG
/// image to extract a single tile. This cache stores the decoded image
/// so subsequent tile requests from the same level are satisfied from
/// memory instead of re-decoding.
pub(super) type FullDecodeCache = SingleFlightTileCache<IfdId>;
pub(super) type NdpiStripCache = SingleFlightTileCache<NdpiStripKey>;
pub(super) type SyntheticLevelCache = SingleFlightTileCache<SyntheticLevelKey>;
pub(super) type SyntheticRegionCache = ByteSizedTileCache<SyntheticLevelKey>;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct NdpiStripKey {
    pub(super) ifd_id: IfdId,
    pub(super) col: u32,
    pub(super) native_row: u32,
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

#[cfg(test)]
#[path = "caches/tests/single_flight.rs"]
mod single_flight_tests;
