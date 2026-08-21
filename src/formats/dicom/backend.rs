use super::*;

pub(super) fn is_encapsulated_transfer_syntax(uid: &str) -> bool {
    uid == JPEG_TRANSFER_SYNTAX
        || uid == RLE_TRANSFER_SYNTAX
        || JP2K_TRANSFER_SYNTAXES.contains(&uid)
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn dicom_jp2k_device_decode_enabled() -> bool {
    crate::core::environment::flag(DICOM_JP2K_DEVICE_DECODE_ENV)
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) const DICOM_CLASSIC_JP2K_PREFER_DEVICE_BATCH_MIN: usize = 8;

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn dicom_htj2k_transfer_syntax(transfer_syntax_uid: &str) -> bool {
    matches!(
        transfer_syntax_uid,
        HTJ2K_TRANSFER_SYNTAX
            | HTJ2K_LOSSLESS_TRANSFER_SYNTAX
            | HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX
    )
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn dicom_jp2k_device_batch_allowed_for_output(
    transfer_syntax_uid: &str,
    output: &TileOutputPreference,
    classic_jp2k_override: bool,
    batch_len: usize,
) -> bool {
    if !JP2K_TRANSFER_SYNTAXES.contains(&transfer_syntax_uid) {
        return false;
    }
    if dicom_htj2k_transfer_syntax(transfer_syntax_uid) {
        return output.compressed_device_decode_enabled();
    }

    output.compressed_device_decode_enabled()
        && (classic_jp2k_override
            || output.requires_device()
            || !output.adaptive_decode_route_enabled()
            || batch_len >= DICOM_CLASSIC_JP2K_PREFER_DEVICE_BATCH_MIN)
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn dicom_jp2k_device_batch_allowed(
    transfer_syntax_uid: &str,
    output: &TileOutputPreference,
    batch_len: usize,
) -> bool {
    dicom_jp2k_device_batch_allowed_for_output(
        transfer_syntax_uid,
        output,
        dicom_jp2k_device_decode_enabled(),
        batch_len,
    )
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

    fn parse_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Arc<DicomSlide>, WsiError> {
        Ok(Arc::new(DicomSlide::parse_with_cache_config(
            path,
            cache_config,
        )?))
    }

    fn open_cached_or_parse(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        let key = FileIdentity::from_path(path)?;
        let slide = match self.probe_cache.take(&key, cache_config) {
            Some(slide) => slide,
            None => self.parse_with_cache_config(path, cache_config)?,
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
        self.probe_with_cache_config(path, CacheConfig::deterministic())
    }
}

impl ConfiguredFormatProbe for DicomBackend {
    fn probe_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<ProbeResult, WsiError> {
        let key = FileIdentity::from_path(path)?;
        if self.probe_cache.get(&key, cache_config).is_some() {
            return Ok(ProbeResult::detected("dicom", ProbeConfidence::Definite));
        }
        if path.is_dir() {
            return match self.parse_with_cache_config(path, cache_config) {
                Ok(slide) => {
                    self.probe_cache.insert(key, cache_config, slide);
                    Ok(ProbeResult::detected("dicom", ProbeConfidence::Definite))
                }
                Err(WsiError::UnsupportedFormat(_)) => Ok(ProbeResult::not_detected("")),
                Err(err) => Err(err),
            };
        }
        match parse_metadata_object(path) {
            Ok(meta) if is_vl_wsi(meta.obj.meta().media_storage_sop_class_uid()) => {
                let slide = self.parse_with_cache_config(path, cache_config)?;
                self.probe_cache.insert(key, cache_config, slide);
                Ok(ProbeResult::detected("dicom", ProbeConfidence::Definite))
            }
            Ok(_) | Err(_) => Ok(ProbeResult::not_detected("")),
        }
    }
}

impl DatasetReader for DicomBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        self.open_cached_or_parse(path, CacheConfig::deterministic())
    }
}

impl ConfiguredDatasetReader for DicomBackend {
    fn open_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        self.open_cached_or_parse(path, cache_config)
    }
}
