//! Optional counters for native benchmarks and scheduling regressions.
//! Ordinary builds compile recording calls away.

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Event {
    CpuJp2kBatches = 0,
    CpuPoolDispatches = 15,
    CpuJp2kTiles = 1,
    Jp2kPreparations = 2,
    #[cfg(feature = "metal")]
    MetalBatchSubmissions = 3,
    #[cfg(feature = "metal")]
    MetalBatchGroups = 4,
    #[cfg(feature = "metal")]
    MetalCompletionWaits = 5,
    #[cfg(feature = "metal")]
    ColorSubmissions = 6,
    #[cfg(feature = "metal")]
    ColorTiles = 7,
    #[cfg(feature = "metal")]
    ReadbackSubmissions = 8,
    #[cfg(feature = "metal")]
    ReadbackBytes = 9,
    #[cfg(feature = "metal")]
    ReadbackStagingBytes = 10,
    #[cfg(feature = "metal")]
    ReadbackQueueCreations = 11,
    #[cfg(feature = "metal")]
    MetalSingleDecodes = 12,
    #[cfg(all(feature = "metal", feature = "route-telemetry"))]
    MetalPrivatePoolPeakBytes = 13,
    #[cfg(all(feature = "metal", feature = "route-telemetry"))]
    MetalSharedPoolPeakBytes = 14,
}

#[cfg(any(test, feature = "route-telemetry"))]
const COUNT: usize = 16;

#[cfg(feature = "route-telemetry")]
static COUNTERS: [std::sync::atomic::AtomicU64; COUNT] =
    [const { std::sync::atomic::AtomicU64::new(0) }; COUNT];

#[cfg(test)]
thread_local! {
    static TEST_COUNTERS: std::cell::Cell<[u64; COUNT]> = const { std::cell::Cell::new([0; COUNT]) };
}

#[inline]
pub(crate) fn record(event: Event, amount: usize) {
    #[cfg(feature = "route-telemetry")]
    COUNTERS[event as usize].fetch_add(amount as u64, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    TEST_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters[event as usize] += amount as u64;
        cell.set(counters);
    });
    let _ = (event, amount);
}

#[cfg(test)]
pub(crate) fn test_count(event: Event) -> u64 {
    TEST_COUNTERS.with(|cell| cell.get()[event as usize])
}

#[cfg(feature = "route-telemetry")]
pub(crate) fn snapshot() -> serde_json::Value {
    let names = [
        "cpu_jp2k_batches",
        "cpu_jp2k_tiles",
        "jp2k_preparations",
        "metal_batch_submissions",
        "metal_batch_groups",
        "metal_completion_waits",
        "color_submissions",
        "color_tiles",
        "readback_submissions",
        "readback_bytes",
        "readback_staging_bytes",
        "readback_queue_creations",
        "metal_single_decodes",
        "metal_private_pool_observed_peak_cached_bytes",
        "metal_shared_pool_observed_peak_cached_bytes",
        "cpu_pool_dispatches",
    ];
    names
        .into_iter()
        .zip(COUNTERS.iter())
        .map(|(name, counter)| {
            (
                name.to_owned(),
                serde_json::json!(counter.load(std::sync::atomic::Ordering::Relaxed)),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

#[cfg(all(feature = "metal", feature = "route-telemetry"))]
pub(crate) fn record_metal_pools(sessions: &crate::output::metal::MetalBackendSessions) {
    match sessions.j2k().buffer_pool_diagnostics() {
        Ok(pools) => {
            for (event, bytes) in [
                (
                    Event::MetalPrivatePoolPeakBytes,
                    pools.private.peak_cached_bytes,
                ),
                (
                    Event::MetalSharedPoolPeakBytes,
                    pools.shared.peak_cached_bytes,
                ),
            ] {
                COUNTERS[event as usize]
                    .fetch_max(bytes as u64, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Err(error) => tracing::debug!(%error, "Metal pool telemetry unavailable"),
    }
}
