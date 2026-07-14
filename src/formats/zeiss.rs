mod attachments;
mod metadata;
mod slide;
mod tiles;

#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::convert::TryFrom;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use czi_rs::{
    AttachmentBlob, CompressionMode as CziCompressionMode, CziFile, Dimension as CziDimension,
    IntRect, PixelType as CziPixelType,
};
use image::imageops::{self, FilterType};
use j2k_core::BackendRequest;
use lru::LruCache;
use std::collections::HashMap as StdHashMap;

use crate::core::hash::Quickhash1;
use crate::core::registry::{
    crop_rgb_interleaved_u8_buffer, read_cpu_tiles_with_backend, DatasetReader, FormatProbe,
    ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::*;
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::error::WsiError;
use crate::properties::Properties;

use slide::{ZeissReader, ZeissSlide};

const FILE_MAGIC: &[u8; 16] = b"ZISRAWFILE\0\0\0\0\0\0";

pub(crate) struct ZeissBackend;

impl ZeissBackend {
    pub(crate) fn new() -> Self {
        Self
    }

    fn parse(&self, path: &Path) -> Result<Arc<ZeissSlide>, WsiError> {
        Ok(Arc::new(ZeissSlide::parse(path)?))
    }
}

impl Default for ZeissBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatProbe for ZeissBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let mut magic = [0u8; 16];
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(_) => {
                return Ok(ProbeResult {
                    detected: false,
                    vendor: String::new(),
                    confidence: ProbeConfidence::Likely,
                });
            }
        };
        if std::io::Read::read_exact(&mut file, &mut magic).is_err() || &magic != FILE_MAGIC {
            return Ok(ProbeResult {
                detected: false,
                vendor: String::new(),
                confidence: ProbeConfidence::Likely,
            });
        }

        Ok(ProbeResult {
            detected: true,
            vendor: "zeiss".into(),
            confidence: ProbeConfidence::Definite,
        })
    }
}

impl DatasetReader for ZeissBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let slide = self.parse(path)?;
        Ok(Box::new(ZeissReader { slide }))
    }
}
