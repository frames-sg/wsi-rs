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

use crate::core::cache::{CacheConfig, PrivateCache, PrivateCacheBudget};
use crate::core::file_identity::FileIdentity;
use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::limits::{read_file_bounded, MAX_COMPRESSED_INPUT_BYTES};
use crate::core::registry::{
    read_cpu_tiles_with_backend, ConfiguredDatasetReader, ConfiguredFormatProbe,
    ConfiguredProbeCache, DatasetReader, FormatProbe, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::*;
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::decode::jpeg::{jpeg_dimensions, JpegTileGeometry};
use crate::error::WsiError;
use crate::formats::companion_path::resolve_companion_file;
use crate::properties::Properties;
use j2k_core::BackendRequest;
#[cfg(test)]
use j2k_jpeg::JpegView as J2kJpegView;
use j2k_jpeg::{
    DecodeRequest as J2kJpegDecodeRequest, Decoder as J2kJpegDecoder, Downscale as J2kDownscale,
    PixelFormat as J2kPixelFormat, Rect as J2kRect,
};

use ini::{parse_vms_ini, GROUP_VMS, KEY_NUM_JPEG_COLS, KEY_NUM_JPEG_ROWS};
use model::VmsSlide;
use slide::VmsReader;

pub(crate) struct HamamatsuVmsBackend {
    probe_cache: ConfiguredProbeCache<VmsSlide>,
}

impl HamamatsuVmsBackend {
    pub(crate) fn new() -> Self {
        Self {
            probe_cache: ConfiguredProbeCache::new(),
        }
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
        let slide = match self.probe_cache.take(&key, cache_config) {
            Some(slide) => slide,
            None => self.parse_with_cache_config(path, cache_config)?,
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
        self.probe_with_cache_config(path, CacheConfig::deterministic())
    }
}

impl ConfiguredFormatProbe for HamamatsuVmsBackend {
    fn probe_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<ProbeResult, WsiError> {
        let ini = match parse_vms_ini(path) {
            Ok(ini) => ini,
            Err(_) => {
                return Ok(ProbeResult::not_detected(""));
            }
        };
        let Some(group) = ini.groups.get(GROUP_VMS) else {
            return Ok(ProbeResult::not_detected(""));
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
            return Ok(ProbeResult::not_detected(""));
        }

        let key = FileIdentity::from_path(path)?;
        if self.probe_cache.get(&key, cache_config).is_some() {
            return Ok(ProbeResult::detected(
                "hamamatsu",
                ProbeConfidence::Definite,
            ));
        }
        let slide = self.parse_with_cache_config(path, cache_config)?;
        self.probe_cache.insert(key, cache_config, slide);

        Ok(ProbeResult::detected(
            "hamamatsu",
            ProbeConfidence::Definite,
        ))
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
