use super::*;

// ── TiffPixelReader ───────────────────────────────────────────────

/// Implements SlideReader by dispatching tile reads based on TileSource type.
/// Holds an Arc<TiffContainer> for concurrent pread access and the layout
/// produced by a TiffLayoutInterpreter.
pub(crate) struct TiffPixelReader {
    pub(super) container: Arc<TiffContainer>,
    pub(super) layout: DatasetLayout,
    pub(super) full_decode_cache: Mutex<FullDecodeCache>,
    pub(super) full_decode_flights: Mutex<HashMap<IfdId, FullDecodeFlight>>,
    pub(super) full_decode_ready: Condvar,
    pub(super) ndpi_strip_cache: Mutex<NdpiStripCache>,
    pub(super) ndpi_mcu_starts_cache: Mutex<NdpiMcuStartsCache>,
    pub(super) ndpi_strip_flights: Mutex<HashMap<NdpiStripKey, NdpiStripFlight>>,
    pub(super) ndpi_strip_ready: Condvar,
    pub(super) synthetic_level_cache: Mutex<SyntheticLevelCache>,
    pub(super) synthetic_region_cache: Mutex<SyntheticLevelCache>,
    pub(super) synthetic_level_flights: Mutex<HashMap<SyntheticLevelKey, SyntheticLevelFlight>>,
    pub(super) synthetic_level_ready: Condvar,
    pub(super) synthetic_prime_once: OnceLock<()>,
    pub(super) stitched_component_tile_cache: Mutex<StitchedComponentTileCache>,
}

impl TiffPixelReader {
    pub(super) fn full_decode_cache_bytes() -> u64 {
        std::env::var(FULL_DECODE_CACHE_BYTES_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_FULL_DECODE_CACHE_BYTES)
    }

    pub(super) fn ndpi_strip_cache_bytes() -> u64 {
        std::env::var(NDPI_STRIP_CACHE_BYTES_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_NDPI_STRIP_CACHE_BYTES)
    }

    pub(super) fn synthetic_level_cache_bytes() -> u64 {
        std::env::var(SYNTHETIC_LEVEL_CACHE_BYTES_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_SYNTHETIC_LEVEL_CACHE_BYTES)
    }

    pub fn new(container: Arc<TiffContainer>, layout: DatasetLayout) -> Self {
        Self {
            container,
            layout,
            full_decode_cache: Mutex::new(FullDecodeCache::new(Self::full_decode_cache_bytes())),
            full_decode_flights: Mutex::new(HashMap::new()),
            full_decode_ready: Condvar::new(),
            ndpi_strip_cache: Mutex::new(NdpiStripCache::new(Self::ndpi_strip_cache_bytes())),
            ndpi_mcu_starts_cache: Mutex::new(HashMap::new()),
            ndpi_strip_flights: Mutex::new(HashMap::new()),
            ndpi_strip_ready: Condvar::new(),
            synthetic_level_cache: Mutex::new(SyntheticLevelCache::new(
                Self::synthetic_level_cache_bytes(),
            )),
            synthetic_region_cache: Mutex::new(SyntheticLevelCache::new(
                Self::synthetic_level_cache_bytes(),
            )),
            synthetic_level_flights: Mutex::new(HashMap::new()),
            synthetic_level_ready: Condvar::new(),
            synthetic_prime_once: OnceLock::new(),
            stitched_component_tile_cache: Mutex::new(StitchedComponentTileCache::default()),
        }
    }
}
