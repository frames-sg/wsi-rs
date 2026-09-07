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
