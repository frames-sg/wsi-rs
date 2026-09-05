use std::sync::{Arc, Mutex};

use crate::core::cache::CacheConfig;
use crate::core::limits::SlideLimits;
use crate::error::WsiError;

/// Configuration which must be identical for a probe parse and its following open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackendOpenConfig {
    pub(crate) cache_config: CacheConfig,
    pub(crate) limits: SlideLimits,
}

impl BackendOpenConfig {
    pub(crate) const fn new(cache_config: CacheConfig, limits: SlideLimits) -> Self {
        Self {
            cache_config,
            limits,
        }
    }

    pub(crate) const fn deterministic() -> Self {
        Self::new(CacheConfig::deterministic(), SlideLimits::default_const())
    }
}

#[derive(Debug, Default)]
struct OpenBudgetState {
    retained_metadata_bytes: u64,
    retained_index_bytes: u64,
}

/// Per-parse accounting shared by every source file in one slide bundle.
#[derive(Debug)]
pub(crate) struct OpenBudget {
    limits: SlideLimits,
    state: Mutex<OpenBudgetState>,
}

impl OpenBudget {
    pub(crate) fn new(limits: SlideLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            state: Mutex::new(OpenBudgetState::default()),
        })
    }

    pub(crate) fn check_metadata_value(&self, bytes: u64) -> Result<(), WsiError> {
        if bytes > self.limits.metadata_value_bytes() {
            return Err(WsiError::ResourceLimit {
                resource: "individual metadata value",
                requested: bytes,
                limit: self.limits.metadata_value_bytes(),
            });
        }
        Ok(())
    }

    pub(crate) const fn limits(&self) -> SlideLimits {
        self.limits
    }

    pub(crate) fn retain_metadata(&self, bytes: u64) -> Result<(), WsiError> {
        self.check_metadata_value(bytes)?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let requested = state.retained_metadata_bytes.saturating_add(bytes);
        if requested > self.limits.aggregate_metadata_bytes() {
            return Err(WsiError::ResourceLimit {
                resource: "aggregate metadata",
                requested,
                limit: self.limits.aggregate_metadata_bytes(),
            });
        }
        state.retained_metadata_bytes = requested;
        Ok(())
    }

    pub(crate) fn retain_index(&self, bytes: u64) -> Result<(), WsiError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let requested = state.retained_index_bytes.saturating_add(bytes);
        if requested > self.limits.tile_index_bytes() {
            return Err(WsiError::ResourceLimit {
                resource: "tile/frame index",
                requested,
                limit: self.limits.tile_index_bytes(),
            });
        }
        state.retained_index_bytes = requested;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_budget_recovers_from_a_poisoned_accounting_mutex() {
        let budget = OpenBudget::new(SlideLimits::default());
        let poisoner = Arc::clone(&budget);
        assert!(std::thread::spawn(move || {
            let _state = poisoner.state.lock().unwrap();
            panic!("poison open budget state");
        })
        .join()
        .is_err());

        budget.retain_metadata(1).unwrap();
        budget.retain_index(1).unwrap();
        let state = budget
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(state.retained_metadata_bytes, 1);
        assert_eq!(state.retained_index_bytes, 1);
    }
}
