use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::core::cache::{CacheConfig, CacheKey, TileCache};
use crate::core::decode_runtime::{AdaptiveDecodeReader, DecodeExecutionOptions, DecodeRuntime};
use crate::core::limits::{ReadWork, SlideAdmission};
use crate::core::types::*;
use crate::error::WsiError;

/// Default maximum region size in pixels. Prevents OOM from unreasonably large
/// region requests (32 megapixels = 128 MiB for RGBA8).
pub(crate) const DEFAULT_MAX_REGION_PIXELS: u64 = 32 * 1024 * 1024;

mod composition;
mod open_config;
mod open_options;
mod probe_cache;
mod registry_impl;
mod slide;
mod traits;

pub use crate::core::limits::{SlideLimitError, SlideLimits};
pub(crate) use composition::{
    check_region_pixel_limit, composite_fractional_region_from_source,
    composite_fractional_region_from_source_streaming, composite_region_from_source,
    composite_region_from_source_in_batches, composite_region_from_source_streaming,
    crop_rgb_interleaved_u8_buffer, read_display_tile_from_source,
};
pub(crate) use open_config::{BackendOpenConfig, OpenBudget};
pub use open_options::SlideOpenOptions;
pub(crate) use probe_cache::ConfiguredProbeCache;
pub use registry_impl::FormatRegistry;
pub use slide::Slide;
pub(crate) use traits::{
    read_cpu_tiles, ConfiguredDatasetReader, ConfiguredFormatProbe, ConservativeManagedReader,
    ManagedSlideReader,
};
pub use traits::{
    DatasetReader, FormatProbe, ProbeConfidence, ProbeResult, SlideReadContext, SlideReader,
};

#[cfg(test)]
mod tests;
