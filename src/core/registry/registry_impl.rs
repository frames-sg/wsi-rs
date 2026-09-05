use super::*;

// ── Arc blanket impls ─────────────────────────────────────────────
// Enable a single Arc<T> to be registered as both FormatProbe and
// DatasetReader when T implements both traits. Used by TiffFamilyBackend.

impl<T: FormatProbe> FormatProbe for Arc<T> {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        (**self).probe(path)
    }
}

impl<T: ConfiguredFormatProbe> ConfiguredFormatProbe for Arc<T> {
    fn probe_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<ProbeResult, WsiError> {
        (**self).probe_with_config(path, config)
    }
}

impl<T: DatasetReader> DatasetReader for Arc<T> {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        (**self).open(path)
    }
}

impl<T: ConfiguredDatasetReader> ConfiguredDatasetReader for Arc<T> {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        (**self).open_with_config(path, config)
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
    probe: Box<RegistryProbe>,
    reader: Box<RegistryReader>,
}

type RegistryProbe =
    dyn Fn(&Path, BackendOpenConfig) -> Result<ProbeResult, WsiError> + Send + Sync;
type RegistryReader =
    dyn Fn(&Path, BackendOpenConfig) -> Result<Box<dyn ManagedSlideReader>, WsiError> + Send + Sync;

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
            probe: Box::new(move |path, _config| probe.probe(path)),
            reader: Box::new(move |path, config| {
                Ok(Box::new(ConservativeManagedReader::new(
                    reader.open(path)?,
                    config.limits.encoded_unit_bytes(),
                )))
            }),
        });
    }

    pub(crate) fn register_cache_configured(
        &mut self,
        probe: impl ConfiguredFormatProbe + 'static,
        reader: impl ConfiguredDatasetReader + 'static,
    ) {
        self.backends.push(RegisteredBackend {
            probe: Box::new(move |path, config| probe.probe_with_config(path, config)),
            reader: Box::new(move |path, config| reader.open_with_config(path, config)),
        });
    }

    /// Probe all backends and return the best detected format without opening it.
    ///
    /// Definite confidence beats Likely. First-registered wins ties.
    pub fn detect_vendor(&self, path: &Path) -> Result<Option<ProbeResult>, WsiError> {
        self.best_probe(path, BackendOpenConfig::deterministic())
            .map(|best| best.map(|(result, _)| result))
    }

    /// Probe all backends, open with best match.
    /// Definite confidence beats Likely. First-registered wins ties.
    pub fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let managed = self.open_with_config(path, BackendOpenConfig::deterministic())?;
        Ok(managed)
    }

    /// Probe all backends and open the best match with the supplied cache policy.
    pub(crate) fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        match self.best_probe(path, config)? {
            Some((_, i)) => (self.backends[i].reader)(path, config),
            None => Err(WsiError::UnsupportedFormat(path.display().to_string())),
        }
    }

    fn best_probe(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Option<(ProbeResult, usize)>, WsiError> {
        let mut best: Option<(ProbeResult, usize)> = None;
        let mut first_error: Option<WsiError> = None;

        for (i, backend) in self.backends.iter().enumerate() {
            match (backend.probe)(path, config) {
                Ok(result) => {
                    if result.detected {
                        if result.confidence == ProbeConfidence::Definite {
                            return Ok(Some((result, i)));
                        }
                        if best.is_none() {
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
