//! FIFO-bounded route decisions with one nonblocking calibration owner per key.
use super::*;
use std::ops::Deref;
#[cfg(any(feature = "metal", feature = "cuda"))]
use std::sync::Condvar;

/// Largest prepared batch a background calibration may retain after its
/// foreground read returns and releases that read's admission.
#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) const BACKGROUND_CALIBRATION_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Default)]
pub(super) struct RouteEntry {
    busy: bool,
    warmed: bool,
    samples: Vec<(Duration, Duration)>,
    decision: Option<DecodeRouteDecision>,
}

pub(super) type DecodeRouteCache = LruCache<DecodeRouteKey, RouteEntry>;

pub(super) fn new_decode_route_cache() -> DecodeRouteCache {
    LruCache::new(NonZeroUsize::new(ROUTE_CACHE_MAX_ENTRIES).expect("nonzero route capacity"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CalibrationStep {
    Warmup,
    /// A foreground comparison alternates which route runs first. Background
    /// calibration always times CPU on the read and the device afterwards.
    Sample {
        cpu_first: bool,
    },
}

/// `R` is how the lease reaches its runtime: a borrow for work on the calling
/// thread, or an `Arc` that a background calibration can own.
pub(super) enum RouteClaim<R: Deref<Target = DecodeRuntime>> {
    Cpu,
    FirstCpu { _lease: CalibrationLease<R> },
    Ready(DecodeRouteDecision),
    Calibrate(CalibrationLease<R>),
}

pub(super) struct CalibrationLease<R: Deref<Target = DecodeRuntime>> {
    runtime: R,
    key: DecodeRouteKey,
    pub(super) step: CalibrationStep,
}

enum ClaimState {
    Cpu,
    FirstCpu,
    Ready(DecodeRouteDecision),
    Calibrate(CalibrationStep),
}

fn claim_route_with<R: Deref<Target = DecodeRuntime>>(
    runtime: R,
    key: DecodeRouteKey,
) -> RouteClaim<R> {
    match runtime.claim_route_state(&key) {
        ClaimState::Cpu => RouteClaim::Cpu,
        ClaimState::Ready(decision) => RouteClaim::Ready(decision),
        ClaimState::FirstCpu => RouteClaim::FirstCpu {
            _lease: CalibrationLease {
                runtime,
                key,
                step: CalibrationStep::Warmup,
            },
        },
        ClaimState::Calibrate(step) => {
            RouteClaim::Calibrate(CalibrationLease { runtime, key, step })
        }
    }
}

impl DecodeRuntime {
    #[cfg(test)]
    pub(super) fn claim_route(&self, key: DecodeRouteKey) -> RouteClaim<&Self> {
        claim_route_with(self, key)
    }

    /// [`Self::claim_route`] with leases that can move to another thread.
    #[cfg(any(feature = "metal", feature = "cuda"))]
    pub(super) fn claim_owned_route(
        self: &Arc<Self>,
        key: DecodeRouteKey,
    ) -> RouteClaim<Arc<Self>> {
        claim_route_with(Arc::clone(self), key)
    }

    /// Marks the route busy for the lease the caller constructs.
    fn claim_route_state(&self, key: &DecodeRouteKey) -> ClaimState {
        let key = key.clone();
        let mut cache = self.route_cache.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.contains(&key) && !key.device_identity.is_empty() {
            let mut pending = key.clone();
            pending.device_identity.clear();
            if let Some(entry) = cache.peek(&pending) {
                // Device initialization can finish while a warmup owns the
                // unresolved key. It alone performs the identity migration.
                if entry.busy {
                    return ClaimState::Cpu;
                }
                let entry = cache.pop(&pending).expect("pending entry exists");
                cache.put(key.clone(), entry);
            }
        }
        if cache
            .peek(&key)
            .is_none_or(|entry| !entry.busy && entry.decision.is_none())
        {
            if let Some(decision) = clipped_cpu_preference(&cache, &key) {
                // Keep the measured full-tile route as the evidence owner.
                // No new decision is published by this optional shortcut.
                return ClaimState::Ready(decision);
            }
        }
        let Some(entry) = cache.peek_mut(&key) else {
            insert_entry(
                &mut cache,
                key.clone(),
                RouteEntry {
                    busy: true,
                    ..RouteEntry::default()
                },
            );
            return if cache.contains(&key) {
                // Protect startup until CPU output is ready, just as later
                // calibration protects its pending route from other callers.
                ClaimState::FirstCpu
            } else {
                ClaimState::Cpu
            };
        };
        if let Some(decision) = &entry.decision {
            return ClaimState::Ready(decision.clone());
        }
        if entry.busy {
            return ClaimState::Cpu;
        }
        entry.busy = true;
        ClaimState::Calibrate(if entry.warmed {
            CalibrationStep::Sample {
                cpu_first: entry.samples.len() % 2 == 0,
            }
        } else {
            CalibrationStep::Warmup
        })
    }

    #[cfg(test)]
    pub(super) fn cached_route(&self, key: &DecodeRouteKey) -> Option<DecodeRouteDecision> {
        self.route_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .peek(key)?
            .decision
            .clone()
    }

    pub(super) fn store_route(
        &self,
        key: DecodeRouteKey,
        decision: DecodeRouteDecision,
        control: Option<&crate::ReadControl>,
    ) -> Result<(), WsiError> {
        let mut cache = self.route_cache.lock().unwrap_or_else(|e| e.into_inner());
        let publish = || {
            if let Some(entry) = cache.peek_mut(&key) {
                entry.decision = Some(decision);
            } else {
                insert_entry(
                    &mut cache,
                    key,
                    RouteEntry {
                        decision: Some(decision),
                        ..RouteEntry::default()
                    },
                );
            }
        };
        if let Some(control) = control {
            control.publish_if_active(publish)
        } else {
            publish();
            Ok(())
        }
    }
}

fn insert_entry(cache: &mut DecodeRouteCache, key: DecodeRouteKey, entry: RouteEntry) {
    if cache.len() == ROUTE_CACHE_MAX_ENTRIES {
        let evict = cache
            .iter()
            .rev()
            .find(|(_, value)| !value.busy)
            .map(|(key, _)| key.clone());
        let Some(evict) = evict else {
            return;
        };
        cache.pop(&evict);
    }
    cache.put(key, entry);
}

fn clipped_cpu_preference(
    cache: &DecodeRouteCache,
    key: &DecodeRouteKey,
) -> Option<DecodeRouteDecision> {
    cache.iter().find_map(|(full, entry)| {
        let decision = entry.decision.as_ref()?;
        let [(geometry, _)] = full.sample_geometry.0.as_slice() else {
            return None;
        };
        // A clipped edge has no more pixel work to amortize device overhead.
        // Be conservative only after a greater-than-fourfold measured loss;
        // larger batches, other levels, codecs, devices and CPU budgets still calibrate.
        (decision.winner == DecodeRoute::Cpu
            && !decision.device_failure
            && !decision.cpu_elapsed.is_zero()
            && decision.device_elapsed > decision.cpu_elapsed.saturating_mul(4)
            && full.dataset_id == key.dataset_id
            && full.scene == key.scene
            && full.series == key.series
            && full.level == key.level
            && full.codec_kind == key.codec_kind
            && full.device_identity == key.device_identity
            && full.cpu_workers == key.cpu_workers
            && full.sample_tile_count == key.sample_tile_count
            && key
                .sample_geometry
                .0
                .iter()
                .all(|(edge, _)| edge.width <= geometry.width && edge.height <= geometry.height))
        .then(|| decision.clone())
    })
}

impl<R: Deref<Target = DecodeRuntime>> CalibrationLease<R> {
    pub(super) fn bind_device(&mut self, identity: String) {
        if self.key.device_identity == identity {
            return;
        }
        let mut cache = self
            .runtime
            .route_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = cache.pop(&self.key).expect("busy route cannot be evicted");
        self.key.device_identity = identity;
        cache.put(self.key.clone(), entry);
    }

    pub(super) fn fail(self, control: Option<&crate::ReadControl>) -> Result<(), WsiError> {
        self.runtime.store_route(
            self.key.clone(),
            DecodeRouteDecision::device_failure(),
            control,
        )
    }

    pub(super) fn complete(
        self,
        sample: Option<(Duration, Duration)>,
        control: Option<&crate::ReadControl>,
    ) -> Result<(), WsiError> {
        let mut cache = self
            .runtime
            .route_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut publish = || {
            let entry = cache
                .peek_mut(&self.key)
                .expect("busy route cannot be evicted");
            match self.step {
                CalibrationStep::Warmup => {
                    entry.warmed = true;
                    // Foreground calibration is optional. A device warmup
                    // costing more than four uncached CPU decodes is already
                    // too expensive for this route; avoid three more probes.
                    // This is deliberately conservative about cold GPU costs.
                    if let Some((cpu, device)) = sample {
                        if !cpu.is_zero() && device > cpu.saturating_mul(4) {
                            entry.decision = Some(DecodeRouteDecision::measured(cpu, device));
                        }
                    }
                }
                CalibrationStep::Sample { .. } => {
                    entry
                        .samples
                        .push(sample.expect("completed comparison has both timings"));
                    if entry.samples.len() == 3 {
                        entry
                            .samples
                            .sort_by(|a, b| ratio(*a).total_cmp(&ratio(*b)));
                        let (cpu, device) = entry.samples[1];
                        entry.decision = Some(DecodeRouteDecision::measured(cpu, device));
                        entry.samples.clear();
                    }
                }
            }
        };
        let result = if let Some(control) = control {
            control.publish_if_active(publish)
        } else {
            publish();
            Ok(())
        };
        // Drop releases ownership after unlocking, including cancelled publication.
        drop(cache);
        result
    }
}

fn ratio((cpu, device): (Duration, Duration)) -> f64 {
    if cpu.is_zero() {
        f64::INFINITY
    } else {
        device.as_secs_f64() / cpu.as_secs_f64()
    }
}

impl<R: Deref<Target = DecodeRuntime>> Drop for CalibrationLease<R> {
    fn drop(&mut self) {
        if let Some(entry) = self
            .runtime
            .route_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .peek_mut(&self.key)
        {
            entry.busy = false;
        }
    }
}

/// Admits one background device calibration per runtime at a time.
#[cfg(any(feature = "metal", feature = "cuda"))]
#[derive(Debug, Default)]
pub(super) struct BackgroundCalibrationSlot {
    busy: Mutex<bool>,
    idle: Condvar,
    #[cfg(test)]
    started: std::sync::atomic::AtomicUsize,
    /// Holds the next background calibration before its device work.
    #[cfg(test)]
    hold: Mutex<Option<Arc<std::sync::Barrier>>>,
}

/// Owns the runtime's background calibration slot until dropped.
#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) struct BackgroundCalibration {
    runtime: Arc<DecodeRuntime>,
}

#[cfg(any(feature = "metal", feature = "cuda"))]
impl DecodeRuntime {
    /// Claims the background slot, or `None` while another calibration runs.
    pub(super) fn claim_background_calibration(self: &Arc<Self>) -> Option<BackgroundCalibration> {
        let slot = &self.background_calibration;
        let mut busy = slot.busy.lock().unwrap_or_else(|e| e.into_inner());
        if *busy {
            return None;
        }
        *busy = true;
        #[cfg(test)]
        slot.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(BackgroundCalibration {
            runtime: Arc::clone(self),
        })
    }

    #[cfg(test)]
    pub(crate) fn background_calibrations_started(&self) -> usize {
        self.background_calibration
            .started
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn hold_next_background_calibration(&self, barrier: Arc<std::sync::Barrier>) {
        *self.background_calibration.hold.lock().unwrap() = Some(barrier);
    }

    #[cfg(test)]
    pub(super) fn wait_at_background_hold(&self) {
        let hold = self.background_calibration.hold.lock().unwrap().take();
        if let Some(barrier) = hold {
            barrier.wait();
        }
    }

    /// Blocks until no background calibration is running.
    #[cfg(test)]
    pub(crate) fn wait_for_background_calibration(&self) {
        let slot = &self.background_calibration;
        let mut busy = slot.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy {
            busy = slot.idle.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[cfg(any(feature = "metal", feature = "cuda"))]
impl Drop for BackgroundCalibration {
    fn drop(&mut self) {
        let slot = &self.runtime.background_calibration;
        *slot.busy.lock().unwrap_or_else(|e| e.into_inner()) = false;
        slot.idle.notify_all();
    }
}
