use std::sync::{Arc, Mutex};

use crate::core::file_identity::FileIdentity;
use crate::core::registry::BackendOpenConfig;

/// Single-use bridge from a cache-configured probe to the following open.
///
/// A single slot avoids eager LRU allocation and prevents multiple parsed
/// slides, each with their own private caches, from accumulating in a backend.
pub(crate) struct ConfiguredProbeCache<T> {
    entry: Mutex<Option<ConfiguredProbeEntry<T>>>,
}

struct ConfiguredProbeEntry<T> {
    identity: FileIdentity,
    config: BackendOpenConfig,
    value: Arc<T>,
}

impl<T> ConfiguredProbeCache<T> {
    pub(crate) fn new() -> Self {
        Self {
            entry: Mutex::new(None),
        }
    }

    pub(crate) fn get(&self, identity: &FileIdentity, config: BackendOpenConfig) -> Option<Arc<T>> {
        self.entry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|entry| entry.identity == *identity && entry.config == config)
            .map(|entry| entry.value.clone())
    }

    pub(crate) fn insert(&self, identity: FileIdentity, config: BackendOpenConfig, value: Arc<T>) {
        *self.entry.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(ConfiguredProbeEntry {
                identity,
                config,
                value,
            });
    }

    pub(crate) fn take(
        &self,
        identity: &FileIdentity,
        config: BackendOpenConfig,
    ) -> Option<Arc<T>> {
        let mut slot = self.entry.lock().unwrap_or_else(|error| error.into_inner());
        if slot
            .as_ref()
            .is_some_and(|entry| entry.identity == *identity && entry.config == config)
        {
            slot.take().map(|entry| entry.value)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests;
