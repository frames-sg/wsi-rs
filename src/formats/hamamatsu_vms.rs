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
use crate::core::limits::read_file_bounded;
#[cfg(test)]
use crate::core::limits::MAX_COMPRESSED_INPUT_BYTES;
use crate::core::registry::{
    read_cpu_tiles, BackendOpenConfig, ConfiguredDatasetReader, ConfiguredFormatProbe,
    ConfiguredProbeCache, ConservativeManagedReader, DatasetReader, FormatProbe,
    ManagedSlideReader, OpenBudget, ProbeConfidence, ProbeResult, SlideReader,
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

use ini::{parse_vms_ini_with_budget, GROUP_VMS, KEY_NUM_JPEG_COLS, KEY_NUM_JPEG_ROWS};
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

    fn parse_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Arc<VmsSlide>, WsiError> {
        Ok(Arc::new(VmsSlide::parse_with_config(path, config)?))
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
        self.probe_with_config(path, BackendOpenConfig::deterministic())
    }
}

impl ConfiguredFormatProbe for HamamatsuVmsBackend {
    fn probe_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<ProbeResult, WsiError> {
        let probe_budget = OpenBudget::new(config.limits);
        let ini = match parse_vms_ini_with_budget(path, probe_budget.as_ref()) {
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
        if self.probe_cache.get(&key, config).is_some() {
            return Ok(ProbeResult::detected(
                "hamamatsu",
                ProbeConfidence::Definite,
            ));
        }
        let slide = self.parse_with_config(path, config)?;
        self.probe_cache.insert(key, config, slide);

        Ok(ProbeResult::detected(
            "hamamatsu",
            ProbeConfidence::Definite,
        ))
    }
}

impl DatasetReader for HamamatsuVmsBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        self.open_cached_or_parse(path, BackendOpenConfig::deterministic())
    }
}

impl ConfiguredDatasetReader for HamamatsuVmsBackend {
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
