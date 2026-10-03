//! Observation and synchronization points that tests drive in production code.
//!
//! Production code calls these hooks unconditionally. Outside `cfg(test)` the
//! types are zero-sized and every hook is an empty inline function, so test
//! state lives here instead of in the modules being tested. Path counters
//! belong in [`crate::core::execution_telemetry`].

#[cfg(all(test, any(feature = "metal", feature = "cuda")))]
use std::sync::atomic::AtomicUsize;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(all(test, any(feature = "metal", feature = "cuda")))]
use std::sync::Mutex;
#[cfg(test)]
use std::sync::{Arc, Barrier};

/// Decode accounting for one slide's source images, plus a rendezvous that
/// tests use to make concurrent cache misses overlap.
#[derive(Debug, Default)]
pub(crate) struct SourceProbe {
    #[cfg(test)]
    decodes: AtomicU64,
    #[cfg(test)]
    prepared_peak_bytes: AtomicU64,
    #[cfg(test)]
    miss_barrier: Option<Arc<Barrier>>,
}

impl SourceProbe {
    #[inline]
    pub(crate) fn record_decode(&self) {
        #[cfg(test)]
        self.decodes.fetch_add(1, Ordering::Relaxed);
    }

    /// Records the decoded bytes one prepared batch holds at once. `bytes`
    /// runs only in test builds.
    #[inline]
    pub(crate) fn record_prepared_bytes(&self, bytes: impl FnOnce() -> u64) {
        #[cfg(test)]
        self.prepared_peak_bytes
            .fetch_max(bytes(), Ordering::Relaxed);
        #[cfg(not(test))]
        let _ = bytes;
    }

    /// Waits at the barrier a test installed before claiming a cache miss.
    #[inline]
    pub(crate) fn wait_before_miss(&self) {
        #[cfg(test)]
        if let Some(barrier) = &self.miss_barrier {
            barrier.wait();
        }
    }
}

#[cfg(test)]
impl SourceProbe {
    pub(crate) fn decodes(&self) -> u64 {
        self.decodes.load(Ordering::Relaxed)
    }

    pub(crate) fn prepared_peak_bytes(&self) -> u64 {
        self.prepared_peak_bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn set_miss_barrier(&mut self, barrier: Arc<Barrier>) {
        self.miss_barrier = Some(barrier);
    }
}

/// Counts entries to a code path and can hold the next entry at a barrier.
#[cfg(any(feature = "metal", feature = "cuda"))]
#[derive(Debug, Default)]
pub(crate) struct PathGate {
    #[cfg(test)]
    entries: AtomicUsize,
    #[cfg(test)]
    hold: Mutex<Option<Arc<Barrier>>>,
}

#[cfg(any(feature = "metal", feature = "cuda"))]
impl PathGate {
    #[inline]
    pub(crate) fn record_entry(&self) {
        #[cfg(test)]
        self.entries.fetch_add(1, Ordering::SeqCst);
    }

    /// Waits at the barrier installed by `hold_next`, once.
    #[inline]
    pub(crate) fn pass(&self) {
        #[cfg(test)]
        {
            let hold = self.hold.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(barrier) = hold {
                barrier.wait();
            }
        }
    }
}

#[cfg(all(test, feature = "metal"))]
impl PathGate {
    pub(crate) fn entries(&self) -> usize {
        self.entries.load(Ordering::SeqCst)
    }

    pub(crate) fn hold_next(&self, barrier: Arc<Barrier>) {
        *self.hold.lock().unwrap_or_else(|e| e.into_inner()) = Some(barrier);
    }
}

#[cfg(test)]
thread_local! {
    static IDLE_CORES_OVERRIDE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The idle-core count a test fixed on this thread with [`with_idle_cores`].
#[inline]
pub(crate) fn idle_cores_override() -> Option<usize> {
    #[cfg(test)]
    return IDLE_CORES_OVERRIDE.with(std::cell::Cell::get);
    #[cfg(not(test))]
    None
}

/// Runs `f` with [`crate::core::batch::idle_cores`] fixed on this thread, so
/// batch planning is deterministic regardless of other tests' work.
#[cfg(test)]
pub(crate) fn with_idle_cores<T>(cores: usize, f: impl FnOnce() -> T) -> T {
    let previous = IDLE_CORES_OVERRIDE.with(|cell| cell.replace(Some(cores)));
    let result = f();
    IDLE_CORES_OVERRIDE.with(|cell| cell.set(previous));
    result
}
