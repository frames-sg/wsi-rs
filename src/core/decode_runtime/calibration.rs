//! FIFO-bounded route decisions with one nonblocking calibration owner per key.
use super::*;

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
    Sample { cpu_first: bool },
}

pub(super) enum RouteClaim<'a> {
    Cpu,
    FirstCpu { _lease: CalibrationLease<'a> },
    Ready(DecodeRouteDecision),
    Calibrate(CalibrationLease<'a>),
}

pub(super) struct CalibrationLease<'a> {
    runtime: &'a DecodeRuntime,
    key: DecodeRouteKey,
    pub(super) step: CalibrationStep,
}

impl DecodeRuntime {
    pub(super) fn claim_route(&self, key: DecodeRouteKey) -> RouteClaim<'_> {
        let mut cache = self.route_cache.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.contains(&key) && !key.device_identity.is_empty() {
            let mut pending = key.clone();
            pending.device_identity.clear();
            if let Some(entry) = cache.peek(&pending) {
                // Device initialization can finish while a warmup owns the
                // unresolved key. It alone performs the identity migration.
                if entry.busy {
                    return RouteClaim::Cpu;
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
                return RouteClaim::Ready(decision);
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
                RouteClaim::FirstCpu {
                    _lease: CalibrationLease {
                        runtime: self,
                        key,
                        step: CalibrationStep::Warmup,
                    },
                }
            } else {
                RouteClaim::Cpu
            };
        };
        if let Some(decision) = &entry.decision {
            return RouteClaim::Ready(decision.clone());
        }
        if entry.busy {
            return RouteClaim::Cpu;
        }
        entry.busy = true;
        let step = if entry.warmed {
            CalibrationStep::Sample {
                cpu_first: entry.samples.len() % 2 == 0,
            }
        } else {
            CalibrationStep::Warmup
        };
        RouteClaim::Calibrate(CalibrationLease {
            runtime: self,
            key,
            step,
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

impl CalibrationLease<'_> {
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

impl Drop for CalibrationLease<'_> {
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
