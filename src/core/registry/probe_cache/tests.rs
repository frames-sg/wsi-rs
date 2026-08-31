use super::*;
use crate::{CacheConfig, SlideLimits};

#[test]
fn configured_probe_cache_recovers_from_poison_and_remains_single_use() {
    let file = tempfile::NamedTempFile::new().expect("temporary identity file");
    let identity = FileIdentity::from_path(file.path()).expect("file identity");
    let config = BackendOpenConfig::deterministic();
    let cache = Arc::new(ConfiguredProbeCache::new());

    let poisoned = Arc::clone(&cache);
    let _ = std::thread::spawn(move || {
        let _guard = poisoned.entry.lock().unwrap();
        panic!("poison configured probe cache");
    })
    .join();

    let value = Arc::new(String::from("parsed"));
    cache.insert(identity.clone(), config, Arc::clone(&value));
    let observed = cache
        .get(&identity, config)
        .expect("poison recovery preserves inserted value");
    assert!(Arc::ptr_eq(&observed, &value));
    let taken = cache
        .take(&identity, config)
        .expect("matching entry is consumed once");
    assert!(Arc::ptr_eq(&taken, &value));
    assert!(cache.take(&identity, config).is_none());
}

#[test]
fn configured_probe_cache_does_not_cross_limit_configurations() {
    let file = tempfile::NamedTempFile::new().expect("temporary identity file");
    let identity = FileIdentity::from_path(file.path()).expect("file identity");
    let permissive = BackendOpenConfig::deterministic();
    let strict = BackendOpenConfig::new(
        CacheConfig::deterministic(),
        SlideLimits::default()
            .with_metadata_value_bytes(1_024)
            .unwrap(),
    );
    let cache = ConfiguredProbeCache::new();

    cache.insert(identity.clone(), permissive, Arc::new("parsed"));

    assert!(cache.get(&identity, strict).is_none());
    assert!(cache.take(&identity, strict).is_none());
    assert!(cache.take(&identity, permissive).is_some());
}
