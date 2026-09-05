mod compound;
mod header;
mod model;
mod mosaic;
mod slide;
mod tags;
mod tiles;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cfb::CompoundFile;
use flate2::read::ZlibDecoder;
use image::ImageFormat;

use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::registry::{
    BackendOpenConfig, ConfiguredDatasetReader, ConfiguredFormatProbe, ConservativeManagedReader,
    DatasetReader, FormatProbe, ManagedSlideReader, OpenBudget, ProbeConfidence, ProbeResult,
    SlideReader,
};
use crate::core::types::*;
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::error::WsiError;
use crate::properties::Properties;

use compound::looks_like_zvi;
use model::ZviSlide;
use slide::ZviReader;

const CFB_MAGIC: &[u8; 8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1";
const MAX_ZVI_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ZVI_STREAMS: usize = 32_768;
const MAX_ZVI_PLANES: usize = 16_384;
const MAX_ZVI_TAGS: usize = 16_384;
const MAX_ZVI_AXIS_INDEX: u32 = 65_535;

pub(crate) struct ZeissZviBackend;

impl FormatProbe for ZeissZviBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let mut magic = [0u8; 8];
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(_) => {
                return Ok(ProbeResult::not_detected(""));
            }
        };
        if file.read_exact(&mut magic).is_err() || magic != *CFB_MAGIC {
            return Ok(ProbeResult::not_detected(""));
        }

        let mut compound = match cfb::open(path) {
            Ok(compound) => compound,
            Err(_) => {
                return Ok(ProbeResult::not_detected(""));
            }
        };
        if !looks_like_zvi(&mut compound) {
            return Ok(ProbeResult::not_detected(""));
        }

        Ok(ProbeResult::detected("zeiss", ProbeConfidence::Definite))
    }
}

impl ConfiguredFormatProbe for ZeissZviBackend {}

impl DatasetReader for ZeissZviBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let reader = self.open_with_config(path, BackendOpenConfig::deterministic())?;
        Ok(reader)
    }
}

impl ConfiguredDatasetReader for ZeissZviBackend {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        let encoded_unit_bytes = config.limits.encoded_unit_bytes();
        let slide = Arc::new(ZviSlide::parse_with_config(path, config)?);
        let reader: Box<dyn SlideReader> = Box::new(ZviReader { slide });
        Ok(Box::new(ConservativeManagedReader::new(
            reader,
            encoded_unit_bytes,
        )))
    }
}
