use super::*;

#[non_exhaustive]
pub struct SlideOpenOptions {
    pub(super) registry: FormatRegistry,
    pub(super) cache_config: CacheConfig,
    pub(super) svcache_policy: crate::formats::svcache::SvcachePolicy,
    pub(super) limits: SlideLimits,
    pub(super) decode_execution_options: DecodeExecutionOptions,
}

impl SlideOpenOptions {
    pub fn deterministic() -> Self {
        Self {
            registry: FormatRegistry::builtin(),
            cache_config: CacheConfig::deterministic(),
            svcache_policy: crate::formats::svcache::SvcachePolicy::Off,
            limits: SlideLimits::default(),
            decode_execution_options: DecodeExecutionOptions::default(),
        }
    }

    pub fn with_cache_config(mut self, cache_config: CacheConfig) -> Self {
        self.cache_config = cache_config;
        self
    }

    pub fn with_svcache_policy(
        mut self,
        svcache_policy: crate::formats::svcache::SvcachePolicy,
    ) -> Self {
        self.svcache_policy = svcache_policy;
        self
    }

    pub fn with_registry(mut self, registry: FormatRegistry) -> Self {
        self.registry = registry;
        self
    }

    pub fn with_max_region_pixels(mut self, max_region_pixels: u64) -> Self {
        self.limits = self.limits.with_region_pixels_compat(max_region_pixels);
        self
    }

    /// Replaces all metadata, output, encoded-input, and transient-work limits.
    ///
    /// Built-in readers apply these limits while probing and parsing, before
    /// untrusted metadata and indexes are allocated. Readers registered through
    /// the public [`FormatRegistry`] extension API are trusted during their
    /// `open` call because [`DatasetReader::open`] has no options parameter;
    /// their normalized metadata, encoded/decode admission, outputs, and final
    /// dataset postconditions are still checked afterward.
    pub fn with_limits(mut self, limits: SlideLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_decode_execution_options(
        mut self,
        decode_execution_options: DecodeExecutionOptions,
    ) -> Self {
        self.decode_execution_options = decode_execution_options;
        self
    }

    pub fn cache_config(&self) -> CacheConfig {
        self.cache_config
    }

    pub fn svcache_policy(&self) -> crate::formats::svcache::SvcachePolicy {
        self.svcache_policy
    }

    pub fn max_region_pixels(&self) -> u64 {
        self.limits.region_pixels()
    }

    pub fn limits(&self) -> SlideLimits {
        self.limits
    }

    pub fn decode_execution_options(&self) -> DecodeExecutionOptions {
        self.decode_execution_options
    }
}

impl Default for SlideOpenOptions {
    fn default() -> Self {
        Self::deterministic()
    }
}
