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
    #[cfg(test)]
    pub(crate) fn new(container: Arc<TiffContainer>, layout: DatasetLayout) -> Self {
        Self::new_with_cache_config(container, layout, crate::CacheConfig::deterministic())
    }

    pub(crate) fn new_with_cache_config(
        container: Arc<TiffContainer>,
        layout: DatasetLayout,
        cache_config: crate::CacheConfig,
    ) -> Self {
        let (full_decode_bytes, ndpi_strip_bytes, ndpi_mcu_bytes, synthetic_bytes) =
            private_cache_budgets(cache_config);
        Self {
            container,
            layout,
            full_decode_cache: FullDecodeCache::new(full_decode_bytes),
            ndpi_strip_cache: NdpiStripCache::new(ndpi_strip_bytes),
            ndpi_mcu_starts_cache: Mutex::new(NdpiMcuStartsCache::new(ndpi_mcu_bytes)),
            synthetic_level_cache: SyntheticLevelCache::new(synthetic_bytes),
            synthetic_region_cache: Mutex::new(SyntheticRegionCache::new(synthetic_bytes)),
        }
    }
}

pub(super) fn private_cache_budgets(cache_config: crate::CacheConfig) -> (u64, u64, u64, u64) {
    let aggregate = cache_config.private_cache_budget_bytes();
    let requested = if cache_config.shared_tile_bytes.is_none() {
        [
            crate::core::environment::positive_u64(
                FULL_DECODE_CACHE_BYTES_ENV,
                DEFAULT_FULL_DECODE_CACHE_BYTES,
            ),
            crate::core::environment::positive_u64(
                NDPI_STRIP_CACHE_BYTES_ENV,
                DEFAULT_NDPI_STRIP_CACHE_BYTES,
            ),
            DEFAULT_NDPI_MCU_STARTS_CACHE_BYTES,
            crate::core::environment::positive_u64(
                SYNTHETIC_LEVEL_CACHE_BYTES_ENV,
                DEFAULT_SYNTHETIC_LEVEL_CACHE_BYTES,
            ),
        ]
    } else {
        let full = aggregate / 2;
        let strip = (aggregate / 32).min(1024 * 1024);
        let mcu = strip;
        [
            full,
            strip,
            mcu,
            aggregate.saturating_sub(full + strip + mcu) / 2,
        ]
    };
    let [full, strip, mcu, synthetic] = clamp_private_cache_budgets(aggregate, requested);
    (full, strip, mcu, synthetic)
}

pub(super) fn clamp_private_cache_budgets(aggregate: u64, requested: [u64; 4]) -> [u64; 4] {
    let requested_total = u128::from(requested[0])
        + u128::from(requested[1])
        + u128::from(requested[2])
        + u128::from(requested[3]) * 2;
    if requested_total <= u128::from(aggregate) {
        return requested;
    }
    if requested_total == 0 {
        return [0; 4];
    }
    requested.map(|bytes| {
        u64::try_from(u128::from(bytes) * u128::from(aggregate) / requested_total)
            .unwrap_or(u64::MAX)
    })
}
