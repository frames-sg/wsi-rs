mod attachments;
mod metadata;
mod preflight;
mod slide;
mod tiles;

#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::convert::TryFrom;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use czi_rs::{
    AttachmentBlob, CompressionMode as CziCompressionMode, CziFile, Dimension as CziDimension,
    IntRect, PixelType as CziPixelType,
};
use image::imageops::{self, FilterType};
use j2k_core::BackendRequest;
use std::collections::HashMap as StdHashMap;

use crate::core::cache::{CacheConfig, PrivateCache};
use crate::core::file_identity::FileIdentity;
use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::limits::{checked_product_to_usize, MAX_DECODED_IMAGE_BYTES};
use crate::core::registry::{
    crop_rgb_interleaved_u8_buffer, read_cpu_tiles_with_backend, ConfiguredDatasetReader,
    ConfiguredFormatProbe, DatasetReader, FormatProbe, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::*;
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::error::WsiError;
use crate::properties::Properties;

use slide::{ZeissReader, ZeissSlide};

const FILE_MAGIC: &[u8; 16] = b"ZISRAWFILE\0\0\0\0\0\0";
const MAX_CZI_METADATA_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CZI_DIRECTORY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CZI_SUBBLOCKS: usize = 1_000_000;
const MAX_CZI_ATTACHMENTS: usize = 1_024;
const MAX_CZI_SCENES: usize = 1_024;
const MAX_CZI_LEVELS: usize = 1_024;
const MAX_CZI_TILE_ASSOCIATIONS: usize = 4_000_000;

pub(crate) struct ZeissBackend;

impl FormatProbe for ZeissBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let mut magic = [0u8; 16];
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(_) => {
                return Ok(ProbeResult::not_detected(""));
            }
        };
        if std::io::Read::read_exact(&mut file, &mut magic).is_err() || &magic != FILE_MAGIC {
            return Ok(ProbeResult::not_detected(""));
        }

        Ok(ProbeResult::detected("zeiss", ProbeConfidence::Definite))
    }
}

impl ConfiguredFormatProbe for ZeissBackend {}

impl DatasetReader for ZeissBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let slide = Arc::new(ZeissSlide::parse(path)?);
        Ok(Box::new(ZeissReader { slide }))
    }
}

impl ConfiguredDatasetReader for ZeissBackend {
    fn open_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        let slide = Arc::new(ZeissSlide::parse_with_cache_config(path, cache_config)?);
        Ok(Box::new(ZeissReader { slide }))
    }
}
