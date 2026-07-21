mod ini;
mod jpeg;
mod levels;
mod model;
mod slide;

#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use j2k_core::BackendRequest;
#[cfg(test)]
use j2k_jpeg::JpegView as J2kJpegView;
use j2k_jpeg::{
    DecodeRequest as J2kJpegDecodeRequest, Decoder as J2kJpegDecoder, Downscale as J2kDownscale,
    PixelFormat as J2kPixelFormat, Rect as J2kRect,
};
use lru::LruCache;

use crate::core::cache::CacheConfig;
use crate::core::file_identity::FileIdentity;
use crate::core::hash::Quickhash1;
use crate::core::registry::{
    read_cpu_tiles_with_backend, ConfiguredDatasetReader, DatasetReader, FormatProbe,
    ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::*;
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::decode::jpeg::{jpeg_dimensions, JpegTileGeometry};
use crate::error::WsiError;
use crate::formats::companion_path::resolve_companion_file;
use crate::properties::Properties;

use ini::{parse_vms_ini, GROUP_VMS, KEY_NUM_JPEG_COLS, KEY_NUM_JPEG_ROWS};
use model::VmsSlide;
use slide::VmsReader;

// Parsed probe entries retain shard indexes and open file owners, not pixels.
const VMS_PROBE_ENTRY_ESTIMATE_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) struct HamamatsuVmsBackend {
    probe_cache: Mutex<LruCache<FileIdentity, Arc<VmsSlide>>>,
}

impl HamamatsuVmsBackend {
    pub(crate) fn new() -> Self {
        Self {
            probe_cache: Mutex::new(LruCache::new(
                CacheConfig::deterministic()
                    .private_entry_capacity(VMS_PROBE_ENTRY_ESTIMATE_BYTES, 1),
            )),
        }
    }

    fn parse(&self, path: &Path) -> Result<Arc<VmsSlide>, WsiError> {
        let slide = Arc::new(VmsSlide::parse(path)?);
        Ok(slide)
    }

    fn parse_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Arc<VmsSlide>, WsiError> {
        Ok(Arc::new(VmsSlide::parse_with_cache_config(
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
        let cached = self
            .probe_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop(&key);
        let slide = if cache_config == CacheConfig::deterministic() {
            match cached {
                Some(slide) => slide,
                None => self.parse_with_cache_config(path, cache_config)?,
            }
        } else {
            self.parse_with_cache_config(path, cache_config)?
        };
        Ok(Box::new(VmsReader { slide }))
    }
}

impl Default for HamamatsuVmsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatProbe for HamamatsuVmsBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let ini = match parse_vms_ini(path) {
            Ok(ini) => ini,
            Err(_) => {
                return Ok(ProbeResult {
                    detected: false,
                    vendor: String::new(),
                    confidence: ProbeConfidence::Likely,
                });
            }
        };
        let Some(group) = ini.groups.get(GROUP_VMS) else {
            return Ok(ProbeResult {
                detected: false,
                vendor: String::new(),
                confidence: ProbeConfidence::Likely,
            });
        };
        let cols = group
            .get(KEY_NUM_JPEG_COLS)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let rows = group
            .get(KEY_NUM_JPEG_ROWS)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        if cols == 0 || rows == 0 {
            return Ok(ProbeResult {
                detected: false,
                vendor: String::new(),
                confidence: ProbeConfidence::Likely,
            });
        }

        let slide = self.parse(path)?;
        let key = FileIdentity::from_path(path)?;
        self.probe_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(key, slide);

        Ok(ProbeResult {
            detected: true,
            vendor: "hamamatsu".into(),
            confidence: ProbeConfidence::Definite,
        })
    }
}

impl DatasetReader for HamamatsuVmsBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        self.open_cached_or_parse(path, CacheConfig::deterministic())
    }
}

impl ConfiguredDatasetReader for HamamatsuVmsBackend {
    fn open_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        self.open_cached_or_parse(path, cache_config)
    }
}
