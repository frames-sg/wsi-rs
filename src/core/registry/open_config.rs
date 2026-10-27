use std::sync::{Arc, Mutex, MutexGuard};

use crate::core::cache::CacheConfig;
use crate::core::limits::{ensure_within, SlideLimits};
use crate::core::types::{Dataset, IccProfileProvenance, TileLayout};
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

impl OpenBudgetState {
    fn retain_metadata(&mut self, limits: SlideLimits, bytes: u64) -> Result<(), WsiError> {
        check_metadata_value(limits, bytes)?;
        self.retained_metadata_bytes = ensure_within(
            "aggregate metadata",
            self.retained_metadata_bytes.saturating_add(bytes),
            limits.aggregate_metadata_bytes(),
        )?;
        Ok(())
    }

    fn retain_index(&mut self, limits: SlideLimits, bytes: u64) -> Result<(), WsiError> {
        self.retained_index_bytes = ensure_within(
            "tile/frame index",
            self.retained_index_bytes.saturating_add(bytes),
            limits.tile_index_bytes(),
        )?;
        Ok(())
    }
}

fn check_metadata_value(limits: SlideLimits, bytes: u64) -> Result<(), WsiError> {
    ensure_within(
        "individual metadata value",
        bytes,
        limits.metadata_value_bytes(),
    )
    .map(drop)
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
        check_metadata_value(self.limits, bytes)
    }

    pub(crate) const fn limits(&self) -> SlideLimits {
        self.limits
    }

    pub(crate) fn retain_metadata(&self, bytes: u64) -> Result<(), WsiError> {
        self.state().retain_metadata(self.limits, bytes)
    }

    pub(crate) fn retain_index(&self, bytes: u64) -> Result<(), WsiError> {
        self.state().retain_index(self.limits, bytes)
    }

    fn state(&self) -> MutexGuard<'_, OpenBudgetState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// Charges an opened dataset's retained metadata and tile index to local
/// counters, so a slide never holds more than `limits` allow.
pub(super) fn validate_dataset_limits(
    dataset: &Dataset,
    limits: SlideLimits,
) -> Result<(), WsiError> {
    let mut budget = OpenBudgetState::default();
    let mut retain =
        |bytes: usize| budget.retain_metadata(limits, u64::try_from(bytes).unwrap_or(u64::MAX));
    for (key, value) in dataset.properties.iter() {
        retain(key.len())?;
        retain(value.len())?;
    }
    for scene in &dataset.scenes {
        retain(scene.id.len())?;
        if let Some(name) = &scene.name {
            retain(name.len())?;
        }
        for series in &scene.series {
            retain(series.id.len())?;
            for channel in &series.channels {
                if let Some(name) = &channel.name {
                    retain(name.len())?;
                }
            }
        }
    }
    for (name, image) in &dataset.associated_images {
        retain(name.len())?;
        retain(image.icc_profile.len())?;
    }
    for profile in dataset.icc_profiles.values() {
        retain(profile.len())?;
    }
    for profile in &dataset.source_icc_profiles {
        retain(profile.bytes.len())?;
        match &profile.provenance {
            IccProfileProvenance::DicomOpticalPath {
                sop_instance_uid,
                optical_path_identifier,
            } => {
                retain(sop_instance_uid.len())?;
                if let Some(identifier) = optical_path_identifier {
                    retain(identifier.len())?;
                }
            }
            IccProfileProvenance::ReaderMetadata { source } => retain(source.len())?,
            IccProfileProvenance::TiffTag { .. } => {}
        }
    }

    let levels = dataset
        .scenes
        .iter()
        .flat_map(|scene| &scene.series)
        .flat_map(|series| &series.levels);
    for level in levels {
        let entries = match &level.tile_layout {
            TileLayout::Regular {
                tiles_across,
                tiles_down,
                ..
            } => tiles_across.checked_mul(*tiles_down).unwrap_or(u64::MAX),
            TileLayout::WholeLevel { .. } => 1,
            TileLayout::Irregular { tiles, .. } => u64::try_from(tiles.len()).unwrap_or(u64::MAX),
        };
        budget.retain_index(limits, entries.saturating_mul(64))?;
    }
    Ok(())
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

    #[test]
    fn dataset_limits_charge_each_value_the_aggregate_and_the_tile_index() {
        use crate::test_support::{regular_rgb_dataset_for_test, RegularLevelForTest};
        let mut dataset = regular_rgb_dataset_for_test(
            crate::core::types::DatasetId::new(1),
            "s",
            "ser",
            RegularLevelForTest {
                dimensions: (4, 4),
                tile_width: 2,
                tile_height: 2,
                tiles_across: 2,
                tiles_down: 2,
            },
        );
        dataset.properties.insert("key", "value");
        let limit = |result: Result<(), WsiError>| match result {
            Err(WsiError::ResourceLimit {
                resource,
                requested,
                limit,
            }) => (resource, requested, limit),
            other => panic!("expected a resource limit, got {other:?}"),
        };
        let limits = SlideLimits::default();

        validate_dataset_limits(&dataset, limits).unwrap();
        assert_eq!(
            limit(validate_dataset_limits(
                &dataset,
                limits.with_metadata_value_bytes(4).unwrap()
            )),
            ("individual metadata value", 5, 4)
        );
        // "key", "value", then scene "s" makes nine bytes.
        assert_eq!(
            limit(validate_dataset_limits(
                &dataset,
                limits.with_aggregate_metadata_bytes(8).unwrap()
            )),
            ("aggregate metadata", 9, 8)
        );
        assert_eq!(
            limit(validate_dataset_limits(
                &dataset,
                limits.with_tile_index_bytes(255).unwrap()
            )),
            ("tile/frame index", 256, 255)
        );
    }
}
