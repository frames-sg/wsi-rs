use super::*;

// ── Arc blanket impls ─────────────────────────────────────────────
// Enable a single Arc<T> to be registered as both FormatProbe and
// DatasetReader when T implements both traits. Used by TiffFamilyBackend.

impl<T: FormatProbe> FormatProbe for Arc<T> {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        (**self).probe(path)
    }
}

impl<T: DatasetReader> DatasetReader for Arc<T> {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        (**self).open(path)
    }
}

impl<T: ConfiguredDatasetReader> ConfiguredDatasetReader for Arc<T> {
    fn open_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        (**self).open_with_cache_config(path, cache_config)
    }
}

// ── Format registry ────────────────────────────────────────────────

#[derive(Default)]
pub struct FormatRegistry {
    backends: Vec<RegisteredBackend>,
}

impl std::fmt::Debug for FormatRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FormatRegistry")
            .field("backend_count", &self.backends.len())
            .finish()
    }
}

struct RegisteredBackend {
    probe: Box<dyn FormatProbe>,
    reader: Box<dyn RegistryDatasetReader>,
}

trait RegistryDatasetReader: Send + Sync {
    fn open_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError>;
}

struct DefaultRegistryReader<T>(T);

impl<T: DatasetReader> RegistryDatasetReader for DefaultRegistryReader<T> {
    fn open_with_cache_config(
        &self,
        path: &Path,
        _cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        self.0.open(path)
    }
}

struct CacheConfiguredRegistryReader<T>(T);

impl<T: ConfiguredDatasetReader> RegistryDatasetReader for CacheConfiguredRegistryReader<T> {
    fn open_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        self.0.open_with_cache_config(path, cache_config)
    }
}

impl FormatRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        probe: impl FormatProbe + 'static,
        reader: impl DatasetReader + 'static,
    ) {
        self.backends.push(RegisteredBackend {
            probe: Box::new(probe),
            reader: Box::new(DefaultRegistryReader(reader)),
        });
    }

    pub(crate) fn register_cache_configured(
        &mut self,
        probe: impl FormatProbe + 'static,
        reader: impl ConfiguredDatasetReader + 'static,
    ) {
        self.backends.push(RegisteredBackend {
            probe: Box::new(probe),
            reader: Box::new(CacheConfiguredRegistryReader(reader)),
        });
    }

    /// Probe all backends and return the best detected format without opening it.
    ///
    /// Definite confidence beats Likely. First-registered wins ties.
    pub fn detect_vendor(&self, path: &Path) -> Result<Option<ProbeResult>, WsiError> {
        self.best_probe(path)
            .map(|best| best.map(|(result, _)| result))
    }

    /// Probe all backends, open with best match.
    /// Definite confidence beats Likely. First-registered wins ties.
    pub fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        self.open_with_cache_config(path, CacheConfig::deterministic())
    }

    /// Probe all backends and open the best match with the supplied cache policy.
    pub(crate) fn open_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        match self.best_probe(path)? {
            Some((_, i)) => self.backends[i]
                .reader
                .open_with_cache_config(path, cache_config),
            None => Err(WsiError::UnsupportedFormat(path.display().to_string())),
        }
    }

    fn best_probe(&self, path: &Path) -> Result<Option<(ProbeResult, usize)>, WsiError> {
        let mut best: Option<(ProbeResult, usize)> = None;
        let mut first_error: Option<WsiError> = None;

        for (i, backend) in self.backends.iter().enumerate() {
            match backend.probe.probe(path) {
                Ok(result) => {
                    if result.detected {
                        let should_replace = match best.as_ref() {
                            None => true,
                            Some((existing, _)) => {
                                existing.confidence == ProbeConfidence::Likely
                                    && result.confidence == ProbeConfidence::Definite
                            }
                        };
                        if should_replace {
                            best = Some((result, i));
                        }
                    }
                }
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }

        match best {
            Some(best) => Ok(Some(best)),
            None => {
                if let Some(err) = first_error {
                    Err(err)
                } else {
                    Ok(None)
                }
            }
        }
    }
}
