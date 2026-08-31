use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

use dicom_dictionary_std::{tags, uids};
use dicom_object::{DefaultDicomObject, OpenFileOptions};
use dicom_parser::dataset::{lazy_read::LazyDataSetReader, LazyDataToken};
use dicom_parser::stateful::decode::StatefulDecode;
use dicom_transfer_syntax_registry::{TransferSyntaxIndex, TransferSyntaxRegistry};
use j2k_core::BackendRequest;

#[cfg(test)]
use crate::core::cache::CacheConfig;
#[cfg(test)]
use crate::core::cache::PrivateCache;
use crate::core::file_identity::FileIdentity;
use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::registry::{
    crop_rgb_interleaved_u8_buffer, BackendOpenConfig, ConfiguredDatasetReader,
    ConfiguredFormatProbe, ConfiguredProbeCache, ConservativeManagedReader, DatasetReader,
    FormatProbe, ManagedSlideReader, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::*;
use crate::error::WsiError;
use crate::properties::Properties;

const LEVEL_IMAGE_TYPES: &[&[&str]] = &[
    &["ORIGINAL", "PRIMARY", "VOLUME", "NONE"],
    &["ORIGINAL", "PRIMARY", "VOLUME", "RESAMPLED"],
    &["DERIVED", "PRIMARY", "VOLUME", "NONE"],
    &["DERIVED", "PRIMARY", "VOLUME", "RESAMPLED"],
];
const LABEL_IMAGE_TYPES: &[&[&str]] = &[
    &["ORIGINAL", "PRIMARY", "LABEL", "NONE"],
    &["DERIVED", "PRIMARY", "LABEL", "NONE"],
];
const OVERVIEW_IMAGE_TYPES: &[&[&str]] = &[
    &["ORIGINAL", "PRIMARY", "OVERVIEW", "NONE"],
    &["DERIVED", "PRIMARY", "OVERVIEW", "NONE"],
];
const THUMBNAIL_IMAGE_TYPES: &[&[&str]] = &[
    &["ORIGINAL", "PRIMARY", "THUMBNAIL", "RESAMPLED"],
    &["DERIVED", "PRIMARY", "THUMBNAIL", "RESAMPLED"],
];
const BASE_ONLY_DICOM_PYRAMID_MESSAGE: &str = "This DICOM WSI contains only a full-resolution base layer and no physical pyramid levels. Open the complete DICOM series/folder, or regenerate the DICOM with DERIVED/PRIMARY/VOLUME/RESAMPLED pyramid instances.";
const BASE_ONLY_GUARD_MIN_TILE_COUNT: u64 = 4_096;
const BASE_ONLY_GUARD_MIN_DIMENSION: u32 = 32_768;
const SUPPORTED_TRANSFER_SYNTAXES: &[&str] = &[
    uids::IMPLICIT_VR_LITTLE_ENDIAN,
    uids::EXPLICIT_VR_LITTLE_ENDIAN,
    EXPLICIT_VR_BIG_ENDIAN_TRANSFER_SYNTAX,
    uids::JPEG_BASELINE8_BIT,
    uids::JPEG_EXTENDED12_BIT,
    JPEG_SPECTRAL_SELECTION_TRANSFER_SYNTAX,
    JPEG_FULL_PROGRESSION_TRANSFER_SYNTAX,
    uids::JPEG_LOSSLESS,
    uids::JPEG_LOSSLESS_SV1,
    uids::JPEG2000_LOSSLESS,
    uids::JPEG2000,
    HTJ2K_TRANSFER_SYNTAX,
    HTJ2K_LOSSLESS_TRANSFER_SYNTAX,
    HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
    uids::RLE_LOSSLESS,
];
#[cfg(test)]
const JPEG_TRANSFER_SYNTAX: &str = uids::JPEG_BASELINE8_BIT;
const JPEG_TRANSFER_SYNTAXES: &[&str] = &[
    uids::JPEG_BASELINE8_BIT,
    uids::JPEG_EXTENDED12_BIT,
    JPEG_SPECTRAL_SELECTION_TRANSFER_SYNTAX,
    JPEG_FULL_PROGRESSION_TRANSFER_SYNTAX,
    uids::JPEG_LOSSLESS,
    uids::JPEG_LOSSLESS_SV1,
];
const RLE_TRANSFER_SYNTAX: &str = uids::RLE_LOSSLESS;
// Retired but still decodable Huffman-coded progressive JPEG syntaxes.
const JPEG_SPECTRAL_SELECTION_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.53";
const JPEG_FULL_PROGRESSION_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.55";
const EXPLICIT_VR_BIG_ENDIAN_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.2";
const HTJ2K_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.203";
const HTJ2K_LOSSLESS_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.201";
const HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.202";
const JP2K_TRANSFER_SYNTAXES: &[&str] = &[
    uids::JPEG2000_LOSSLESS,
    uids::JPEG2000,
    HTJ2K_TRANSFER_SYNTAX,
    HTJ2K_LOSSLESS_TRANSFER_SYNTAX,
    HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
];
mod backend;
mod decode;
mod frame_index;
mod image;
mod manifest;
mod metadata;
mod preflight;
mod reader;

pub(crate) use backend::DicomBackend;
use backend::*;
use decode::*;
#[cfg(test)]
use frame_index::*;
use image::*;
use manifest::*;
use metadata::*;
use preflight::*;
use reader::*;

#[cfg(test)]
mod tests;
