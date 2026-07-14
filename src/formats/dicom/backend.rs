use super::*;

pub(super) fn is_encapsulated_transfer_syntax(uid: &str) -> bool {
    uid == JPEG_TRANSFER_SYNTAX
        || uid == RLE_TRANSFER_SYNTAX
        || JP2K_TRANSFER_SYNTAXES.contains(&uid)
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn dicom_jp2k_device_decode_enabled() -> bool {
    std::env::var(DICOM_JP2K_DEVICE_DECODE_ENV).is_ok_and(|value| {
        value.eq_ignore_ascii_case("1")
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("yes")
            || value.eq_ignore_ascii_case("on")
    })
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
    pub(super) probe_cache: Mutex<LruCache<FileIdentity, Arc<DicomSlide>>>,
}

impl DicomBackend {
    pub(crate) fn new() -> Self {
        Self {
            probe_cache: Mutex::new(LruCache::new(NonZeroUsize::new(4).unwrap())),
        }
    }

    pub(super) fn cache_key(path: &Path) -> Result<FileIdentity, WsiError> {
        FileIdentity::from_path(path)
    }

    pub(super) fn parse(&self, path: &Path) -> Result<Arc<DicomSlide>, WsiError> {
        Ok(Arc::new(DicomSlide::parse(path)?))
    }
}

impl Default for DicomBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatProbe for DicomBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let key = Self::cache_key(path)?;
        if self
            .probe_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .is_some()
        {
            return Ok(ProbeResult {
                detected: true,
                vendor: "dicom".into(),
                confidence: ProbeConfidence::Definite,
            });
        }
        if path.is_dir() {
            return match self.parse(path) {
                Ok(slide) => {
                    self.probe_cache
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .put(key, slide);
                    Ok(ProbeResult {
                        detected: true,
                        vendor: "dicom".into(),
                        confidence: ProbeConfidence::Definite,
                    })
                }
                Err(WsiError::UnsupportedFormat(_)) => Ok(ProbeResult {
                    detected: false,
                    vendor: String::new(),
                    confidence: ProbeConfidence::Likely,
                }),
                Err(err) => Err(err),
            };
        }
        match parse_metadata_object(path) {
            Ok(meta) if is_vl_wsi(meta.obj.meta().media_storage_sop_class_uid()) => {
                let slide = self.parse(path)?;
                self.probe_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .put(key, slide);
                Ok(ProbeResult {
                    detected: true,
                    vendor: "dicom".into(),
                    confidence: ProbeConfidence::Definite,
                })
            }
            Ok(_) => Ok(ProbeResult {
                detected: false,
                vendor: String::new(),
                confidence: ProbeConfidence::Likely,
            }),
            Err(_) => Ok(ProbeResult {
                detected: false,
                vendor: String::new(),
                confidence: ProbeConfidence::Likely,
            }),
        }
    }
}

impl DatasetReader for DicomBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let key = Self::cache_key(path)?;
        let cached = self
            .probe_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop(&key);
        let slide = match cached {
            Some(slide) => slide,
            None => self.parse(path)?,
        };
        Ok(Box::new(DicomReader { slide }))
    }
}
