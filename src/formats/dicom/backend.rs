use super::*;
use crate::core::registry::OpenBudget;

pub(super) fn is_encapsulated_transfer_syntax(uid: &str) -> bool {
    is_jpeg_transfer_syntax(uid)
        || uid == RLE_TRANSFER_SYNTAX
        || JP2K_TRANSFER_SYNTAXES.contains(&uid)
}

pub(super) fn is_jpeg_transfer_syntax(uid: &str) -> bool {
    JPEG_TRANSFER_SYNTAXES.contains(&uid)
}

pub(super) fn is_lossless_jpeg_transfer_syntax(uid: &str) -> bool {
    matches!(uid, uids::JPEG_LOSSLESS | uids::JPEG_LOSSLESS_SV1)
}

pub(crate) struct DicomBackend {
    pub(super) probe_cache: ConfiguredProbeCache<DicomSlide>,
}

impl DicomBackend {
    pub(crate) fn new() -> Self {
        Self {
            probe_cache: ConfiguredProbeCache::new(),
        }
    }

    fn parse_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Arc<DicomSlide>, WsiError> {
        Ok(Arc::new(DicomSlide::parse_with_config(path, config)?))
    }

    fn open_cached_or_parse(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        let key = FileIdentity::from_path(path)?;
        let slide = match self.probe_cache.take(&key, config) {
            Some(slide) => slide,
            None => self.parse_with_config(path, config)?,
        };
        Ok(Box::new(DicomReader { slide }))
    }
}

impl Default for DicomBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatProbe for DicomBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        self.probe_with_config(path, BackendOpenConfig::deterministic())
    }
}

impl ConfiguredFormatProbe for DicomBackend {
    fn probe_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<ProbeResult, WsiError> {
        let key = FileIdentity::from_path(path)?;
        if self.probe_cache.get(&key, config).is_some() {
            return Ok(ProbeResult::detected("dicom", ProbeConfidence::Definite));
        }
        if path.is_dir() {
            return match self.parse_with_config(path, config) {
                Ok(slide) => {
                    self.probe_cache.insert(key, config, slide);
                    Ok(ProbeResult::detected("dicom", ProbeConfidence::Definite))
                }
                Err(WsiError::UnsupportedFormat(_)) => Ok(ProbeResult::not_detected("")),
                Err(err) => Err(err),
            };
        }
        let budget = OpenBudget::new(config.limits);
        match parse_metadata_object_with_budget(path, budget.as_ref()) {
            Ok(meta) if is_vl_wsi(meta.obj.meta().media_storage_sop_class_uid()) => {
                let slide = self.parse_with_config(path, config)?;
                self.probe_cache.insert(key, config, slide);
                Ok(ProbeResult::detected("dicom", ProbeConfidence::Definite))
            }
            Ok(_) | Err(_) => Ok(ProbeResult::not_detected("")),
        }
    }
}

impl DatasetReader for DicomBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        self.open_cached_or_parse(path, BackendOpenConfig::deterministic())
    }
}

impl ConfiguredDatasetReader for DicomBackend {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        Ok(Box::new(ConservativeManagedReader::new(
            self.open_cached_or_parse(path, config)?,
            config.limits.encoded_unit_bytes(),
        )))
    }
}
