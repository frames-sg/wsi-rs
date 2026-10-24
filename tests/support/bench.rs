//! Shared helpers for the native benchmark tests.

use sha2::{Digest, Sha256};
use wsi_rs::{CacheConfig, CpuTile, DecodeAcceleration, DecodeExecutionOptions, SlideOpenOptions};

/// The cache profiles every native benchmark measures, by recorded name.
pub(crate) const CACHE_PROFILES: [&str; 3] = ["default", "disabled", "small"];

/// The cache configuration a recorded profile name stands for.
pub(crate) fn cache_profile(name: &str) -> CacheConfig {
    match name {
        "default" => CacheConfig::default(),
        "disabled" => CacheConfig::default()
            .with_shared_tile_bytes(0)
            .with_display_tile_bytes(0),
        "small" => CacheConfig::default()
            .with_shared_tile_bytes(1024 * 1024)
            .with_display_tile_bytes(0),
        _ => panic!("unknown cache profile {name:?}"),
    }
}

/// Process-wide decode route counters, or null without `route-telemetry`.
pub(crate) fn execution_counters() -> serde_json::Value {
    #[cfg(feature = "route-telemetry")]
    {
        serde_json::from_str(&wsi_rs::decode_route_telemetry_json()).unwrap()
    }
    #[cfg(not(feature = "route-telemetry"))]
    {
        serde_json::Value::Null
    }
}

/// SHA-256 over each 8-bit tile's width, height and samples, in order.
pub(crate) fn tile_checksum(tiles: &[CpuTile]) -> String {
    let mut digest = Sha256::new();
    for tile in tiles {
        digest.update(tile.width().to_le_bytes());
        digest.update(tile.height().to_le_bytes());
        digest.update(tile.data().as_u8().unwrap());
    }
    format!("{:x}", digest.finalize())
}

/// Open options with `cache` and the given decode acceleration.
pub(crate) fn open_options(
    cache: CacheConfig,
    acceleration: DecodeAcceleration,
) -> SlideOpenOptions {
    SlideOpenOptions::default()
        .with_cache_config(cache)
        .with_decode_execution_options(
            DecodeExecutionOptions::default().with_acceleration(acceleration),
        )
}

/// Open options with `cache` that decode on the CPU only.
pub(crate) fn cpu_only_options(cache: CacheConfig) -> SlideOpenOptions {
    open_options(cache, DecodeAcceleration::CpuOnly)
}
