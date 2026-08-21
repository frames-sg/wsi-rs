use super::*;

// ── TiffPixelReader ───────────────────────────────────────────────

/// Implements SlideReader by dispatching tile reads based on TileSource type.
/// Holds an Arc<TiffContainer> for concurrent pread access and the layout
/// produced by a TiffLayoutInterpreter.
pub(crate) struct TiffPixelReader {
    pub(super) container: Arc<TiffContainer>,
    pub(super) layout: DatasetLayout,
    pub(super) full_decode_cache: FullDecodeCache,
    pub(super) ndpi_strip_cache: NdpiStripCache,
    pub(super) ndpi_mcu_starts_cache: Mutex<NdpiMcuStartsCache>,
    pub(super) synthetic_level_cache: SyntheticLevelCache,
    pub(super) synthetic_region_cache: Mutex<SyntheticRegionCache>,
}

impl TiffPixelReader {
    pub(super) fn full_decode_cache_bytes() -> u64 {
        crate::core::environment::positive_u64(
            FULL_DECODE_CACHE_BYTES_ENV,
            DEFAULT_FULL_DECODE_CACHE_BYTES,
        )
    }

    pub(super) fn ndpi_strip_cache_bytes() -> u64 {
        crate::core::environment::positive_u64(
            NDPI_STRIP_CACHE_BYTES_ENV,
            DEFAULT_NDPI_STRIP_CACHE_BYTES,
        )
    }

    pub(super) fn synthetic_level_cache_bytes() -> u64 {
        crate::core::environment::positive_u64(
            SYNTHETIC_LEVEL_CACHE_BYTES_ENV,
            DEFAULT_SYNTHETIC_LEVEL_CACHE_BYTES,
        )
    }

    pub(crate) fn new(container: Arc<TiffContainer>, layout: DatasetLayout) -> Self {
        Self {
            container,
            layout,
            full_decode_cache: FullDecodeCache::new(Self::full_decode_cache_bytes()),
            ndpi_strip_cache: NdpiStripCache::new(Self::ndpi_strip_cache_bytes()),
            ndpi_mcu_starts_cache: Mutex::new(HashMap::new()),
            synthetic_level_cache: SyntheticLevelCache::new(Self::synthetic_level_cache_bytes()),
            synthetic_region_cache: Mutex::new(SyntheticRegionCache::new(
                Self::synthetic_level_cache_bytes(),
            )),
        }
    }
}
