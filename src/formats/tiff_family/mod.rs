pub(crate) mod container;
pub(crate) mod error;
pub(crate) mod icc;
pub(crate) mod layout;
pub(crate) mod pixel_access;
#[cfg(test)]
pub(crate) mod test_support;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::core::cache::CacheConfig;
use crate::core::file_identity::FileIdentity;
use crate::core::registry::{
    BackendOpenConfig, ConfiguredDatasetReader, ConfiguredFormatProbe, ConfiguredProbeCache,
    ConservativeManagedReader, DatasetReader, FormatProbe, ManagedSlideReader, OpenBudget,
    ProbeConfidence, ProbeResult, SlideReader,
};
use crate::error::WsiError;
use tracing::debug;

use self::container::TiffContainer;
use self::layout::aperio::AperioInterpreter;
use self::layout::argos::ArgosInterpreter;
use self::layout::generic::GenericTiffInterpreter;
use self::layout::huron::HuronInterpreter;
use self::layout::leica::LeicaInterpreter;
use self::layout::ndpi::NdpiInterpreter;
use self::layout::philips::PhilipsInterpreter;
use self::layout::trestle::TrestleInterpreter;
use self::layout::ventana::VentanaInterpreter;
use self::layout::TiffLayoutInterpreter;
use self::pixel_access::TiffPixelReader;

// ── TiffFamilyBackend ────────────────────────────────────────────────

/// Backend that handles all TIFF-based WSI formats. Implements both
/// `FormatProbe` (detection) and `DatasetReader` (opening) traits.
///
/// Probing does a full `TiffContainer::open()` which parses the entire
/// IFD chain. The parsed container is cached so `open()` doesn't
/// redundantly re-parse — the amortized cost of probe+open is a single parse.
pub(crate) struct TiffFamilyBackend {
    probe_cache: ConfiguredProbeCache<TiffContainer>,
    interpreters: Vec<Box<dyn TiffLayoutInterpreter>>,
}

impl TiffFamilyBackend {
    pub(crate) fn new() -> Self {
        Self {
            // A successful registry open consumes this entry immediately.
            // Retain only the most recent detect-only parse so repeated vendor
            // probes cannot pin many hostile 128 MiB metadata/index budgets.
            probe_cache: ConfiguredProbeCache::new(),
            interpreters: vec![
                Box::new(NdpiInterpreter),
                Box::new(ArgosInterpreter),
                Box::new(HuronInterpreter),
                Box::new(AperioInterpreter),
                Box::new(LeicaInterpreter),
                Box::new(PhilipsInterpreter),
                Box::new(TrestleInterpreter),
                Box::new(VentanaInterpreter),
                Box::new(GenericTiffInterpreter), // must be last — catches any tiled TIFF
            ],
        }
    }

    /// Find the first interpreter that detects the given container.
    fn find_interpreter(&self, container: &TiffContainer) -> Option<&dyn TiffLayoutInterpreter> {
        self.interpreters
            .iter()
            .find(|i| i.detect(container))
            .map(|i| i.as_ref())
    }
}

impl FormatProbe for TiffFamilyBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        self.probe_with_config(path, BackendOpenConfig::deterministic())
    }
}

impl ConfiguredFormatProbe for TiffFamilyBackend {
    fn probe_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<ProbeResult, WsiError> {
        // Parse the full container (IFD chain).
        // If the file isn't a valid TIFF, return detected=false — not an error.
        // This lets other backends in the registry try their probes.
        let budget = OpenBudget::new(config.limits);
        let container = match TiffContainer::open_with_budget(path, budget) {
            Ok(c) => c,
            Err(err) => {
                if matches!(err, error::TiffParseError::ResourceLimit { .. }) {
                    return Err(err.into_wsi_error(path));
                }
                if path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("ndpi"))
                {
                    return Err(err.into_wsi_error(path));
                }
                return Ok(ProbeResult::not_detected(""));
            }
        };

        // Try each interpreter's detect() against the parsed container
        if let Some(interp) = self.find_interpreter(&container) {
            let vendor = interp.vendor_name().to_string();
            // Cache the container for open() to consume
            let key = FileIdentity::from_path(path)?;
            self.probe_cache.insert(key, config, Arc::new(container));

            Ok(ProbeResult::detected(vendor, ProbeConfidence::Definite))
        } else {
            // No interpreter matched — container dropped, not cached
            Ok(ProbeResult::not_detected(""))
        }
    }
}

impl TiffFamilyBackend {
    fn open_configured(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        let started = Instant::now();
        let key = FileIdentity::from_path(path)?;

        // Try to consume the cached container from probe()
        let cached_container = { self.probe_cache.take(&key, config) };
        let cache_hit = cached_container.is_some();

        // If cache miss (e.g., open() called without prior probe()), re-parse
        let container = match cached_container {
            Some(c) => c,
            None => {
                let budget = OpenBudget::new(config.limits);
                let c = TiffContainer::open_with_budget(path, budget)
                    .map_err(|e| e.into_wsi_error(path))?;
                Arc::new(c)
            }
        };

        // Find matching interpreter
        let interpreter = self.find_interpreter(&container).ok_or_else(|| {
            WsiError::UnsupportedFormat(format!(
                "no TIFF layout interpreter detected for: {}",
                path.display(),
            ))
        })?;

        // Interpret the container → DatasetLayout
        let interpret_started = Instant::now();
        let layout = interpreter
            .interpret(&container)
            .map_err(|e| e.into_wsi_error(path))?;

        debug!(
            path = %path.display(),
            vendor = interpreter.vendor_name(),
            cache_hit,
            interpret_elapsed_ms = interpret_started.elapsed().as_secs_f64() * 1000.0,
            open_elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "interpreted TIFF dataset layout"
        );

        // Build pixel reader
        let reader = TiffPixelReader::new_with_cache_config(container, layout, config.cache_config);
        Ok(Box::new(reader))
    }
}

impl DatasetReader for TiffFamilyBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        self.open_configured(
            path,
            BackendOpenConfig::new(CacheConfig::deterministic(), crate::SlideLimits::default()),
        )
    }
}

impl ConfiguredDatasetReader for TiffFamilyBackend {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        Ok(Box::new(ConservativeManagedReader::new(
            self.open_configured(path, config)?,
            config.limits.encoded_unit_bytes(),
        )))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
