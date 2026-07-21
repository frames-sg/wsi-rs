use std::sync::{Arc, Mutex};

use crate::core::cache::CacheConfig;
use crate::core::file_identity::FileIdentity;

/// Single-use bridge from a cache-configured probe to the following open.
///
/// A single slot avoids eager LRU allocation and prevents multiple parsed
/// slides, each with their own private caches, from accumulating in a backend.
pub(crate) struct ConfiguredProbeCache<T> {
    entry: Mutex<Option<ConfiguredProbeEntry<T>>>,
}

struct ConfiguredProbeEntry<T> {
    identity: FileIdentity,
    cache_config: CacheConfig,
    value: Arc<T>,
}

impl<T> ConfiguredProbeCache<T> {
    pub(crate) fn new() -> Self {
        Self {
            entry: Mutex::new(None),
        }
    }

    pub(crate) fn get(&self, identity: &FileIdentity, cache_config: CacheConfig) -> Option<Arc<T>> {
        self.entry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|entry| entry.identity == *identity && entry.cache_config == cache_config)
            .map(|entry| entry.value.clone())
    }

    pub(crate) fn insert(&self, identity: FileIdentity, cache_config: CacheConfig, value: Arc<T>) {
        *self.entry.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(ConfiguredProbeEntry {
                identity,
                cache_config,
                value,
            });
    }

    pub(crate) fn take(
        &self,
        identity: &FileIdentity,
        cache_config: CacheConfig,
    ) -> Option<Arc<T>> {
        let mut slot = self.entry.lock().unwrap_or_else(|error| error.into_inner());
        if slot
            .as_ref()
            .is_some_and(|entry| entry.identity == *identity && entry.cache_config == cache_config)
        {
            slot.take().map(|entry| entry.value)
        } else {
            None
        }
    }
}
