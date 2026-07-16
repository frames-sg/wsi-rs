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
    reader: Box<dyn DatasetReader>,
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
            reader: Box::new(reader),
        });
    }

    /// Create a registry with all built-in backends registered.
    pub fn builtin() -> Self {
        let mut reg = Self::new();
        let svcache = Arc::new(SvcacheBackend);
        reg.register(svcache.clone(), svcache);
        reg.register_native_backends();
        reg
    }

    pub(crate) fn builtin_native() -> Self {
        let mut reg = Self::new();
        reg.register_native_backends();
        reg
    }

    fn register_native_backends(&mut self) {
        let dicom = Arc::new(DicomBackend::new());
        self.register(dicom.clone(), dicom);
        let mirax = Arc::new(MiraxBackend::new());
        self.register(mirax.clone(), mirax);
        let vms = Arc::new(HamamatsuVmsBackend::new());
        self.register(vms.clone(), vms);
        let vsi = Arc::new(OlympusVsiBackend);
        self.register(vsi.clone(), vsi);
        let raw_jp2k = Arc::new(RawJp2kBackend);
        self.register(raw_jp2k.clone(), raw_jp2k);
        let zeiss_zvi = Arc::new(ZeissZviBackend);
        self.register(zeiss_zvi.clone(), zeiss_zvi);
        let zeiss = Arc::new(ZeissBackend);
        self.register(zeiss.clone(), zeiss);
        let tiff = Arc::new(TiffFamilyBackend::new());
        self.register(tiff.clone(), tiff);
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
        match self.best_probe(path)? {
            Some((_, i)) => self.backends[i].reader.open(path),
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
