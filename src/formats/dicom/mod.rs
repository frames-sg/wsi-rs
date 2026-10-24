use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dicom_dictionary_std::{tags, uids};
use dicom_object::{DefaultDicomObject, OpenFileOptions};
use dicom_parser::dataset::{lazy_read::LazyDataSetReader, LazyDataToken};
use dicom_parser::stateful::decode::StatefulDecode;
use dicom_transfer_syntax_registry::{TransferSyntaxIndex, TransferSyntaxRegistry};
use j2k_core::BackendRequest;

use crate::core::file_identity::FileIdentity;
use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::registry::{
    crop_rgb_interleaved_buffer, BackendOpenConfig, ConfiguredDatasetReader, ConfiguredFormatProbe,
    ConfiguredProbeCache, DatasetReader, FormatProbe, ManagedSlideReader, ProbeConfidence,
    ProbeResult, SlideReader,
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
mod backend;
mod decode;
mod frame_index;
mod image;
mod manifest;
mod metadata;
mod preflight;
mod reader;
mod transfer_syntax;

pub(crate) use backend::DicomBackend;
use decode::*;
use image::*;
use manifest::*;
use metadata::*;
use preflight::*;
use reader::*;

#[cfg(test)]
mod tests;
