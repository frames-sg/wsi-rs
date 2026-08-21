pub(crate) mod container;
pub(crate) mod error;
pub(crate) mod icc;
pub(crate) mod layout;
pub(crate) mod pixel_access;
#[cfg(test)]
pub(crate) mod test_support;

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lru::LruCache;

use crate::core::file_identity::FileIdentity;
use crate::core::registry::{
    DatasetReader, FormatProbe, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::error::WsiError;
use tracing::debug;

use self::container::TiffContainer;
use self::layout::aperio::AperioInterpreter;
use self::layout::generic::GenericTiffInterpreter;
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
    probe_cache: Mutex<LruCache<FileIdentity, Arc<TiffContainer>>>,
    interpreters: Vec<Box<dyn TiffLayoutInterpreter>>,
}

impl TiffFamilyBackend {
    pub(crate) fn new() -> Self {
        Self {
            probe_cache: Mutex::new(LruCache::new(NonZeroUsize::new(16).unwrap())),
            interpreters: vec![
                Box::new(NdpiInterpreter),
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
        // Parse the full container (IFD chain).
        // If the file isn't a valid TIFF, return detected=false — not an error.
        // This lets other backends in the registry try their probes.
        let container = match TiffContainer::open(path) {
            Ok(c) => c,
            Err(err) => {
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
            let mut cache = self.probe_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.put(key, Arc::new(container));

            Ok(ProbeResult::detected(vendor, ProbeConfidence::Definite))
        } else {
            // No interpreter matched — container dropped, not cached
            Ok(ProbeResult::not_detected(""))
        }
    }
}

impl DatasetReader for TiffFamilyBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let started = Instant::now();
        let key = FileIdentity::from_path(path)?;

        // Try to consume the cached container from probe()
        let cached_container = {
            let mut cache = self.probe_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.pop(&key)
        };
        let cache_hit = cached_container.is_some();

        // If cache miss (e.g., open() called without prior probe()), re-parse
        let container = match cached_container {
            Some(c) => c,
            None => {
                let c = TiffContainer::open(path).map_err(|e| e.into_wsi_error(path))?;
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
        let reader = TiffPixelReader::new(container, layout);
        Ok(Box::new(reader))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
