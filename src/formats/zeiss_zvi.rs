mod compound;
mod header;
mod model;
mod mosaic;
mod slide;
mod tags;
mod tiles;

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cfb::CompoundFile;
use flate2::read::ZlibDecoder;
use image::ImageFormat;

use crate::core::hash::Quickhash1;
use crate::core::registry::{
    DatasetReader, FormatProbe, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::*;
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::error::WsiError;
use crate::properties::Properties;

use compound::looks_like_zvi;
use model::ZviSlide;
use slide::ZviReader;

const CFB_MAGIC: &[u8; 8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1";

pub(crate) struct ZeissZviBackend;

impl ZeissZviBackend {
    pub(crate) fn new() -> Self {
        Self
    }

    fn parse(&self, path: &Path) -> Result<Arc<ZviSlide>, WsiError> {
        Ok(Arc::new(ZviSlide::parse(path)?))
    }
}

impl Default for ZeissZviBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatProbe for ZeissZviBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let mut magic = [0u8; 8];
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(_) => {
                return Ok(ProbeResult {
                    detected: false,
                    vendor: String::new(),
                    confidence: ProbeConfidence::Likely,
                });
            }
        };
        if file.read_exact(&mut magic).is_err() || magic != *CFB_MAGIC {
            return Ok(ProbeResult {
                detected: false,
                vendor: String::new(),
                confidence: ProbeConfidence::Likely,
            });
        }

        let mut compound = match cfb::open(path) {
            Ok(compound) => compound,
            Err(_) => {
                return Ok(ProbeResult {
                    detected: false,
                    vendor: String::new(),
                    confidence: ProbeConfidence::Likely,
                });
            }
        };
        if !looks_like_zvi(&mut compound) {
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

impl DatasetReader for ZeissZviBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let slide = self.parse(path)?;
        Ok(Box::new(ZviReader { slide }))
    }
}
