use super::*;

use super::frame_index::batch_io::{
    copy_fragments_from_window, DicomFrameReadGroup, DicomFrameReadSpan,
};
use super::frame_index::model::{DicomExtendedOffsetTables, DicomFragmentRef};
use super::frame_index::offset_tables::{
    build_encapsulated_frame_index, checked_padded_fragment_len,
    frame_ranges_from_extended_offsets, read_basic_offset_table_at, read_extended_offset_tables_le,
    read_extended_offset_tables_with_reader, validate_basic_offset_table_len,
};
use super::frame_index::raw_little_endian::{
    read_exact_at, scan_encapsulated_frames_raw_little_endian_controlled,
    scan_raw_encapsulated_pixel_sequence_with_reader_controlled,
};
use super::frame_index::validation::preflight_compressed_frame;
use super::frame_index::*;
use crate::core::cache::{CacheConfig, PrivateCache};
use crate::core::registry::OpenBudget;
use crate::SlideLimits;
use std::sync::Mutex;

use crate::core::registry::Slide;
use dicom_core::value::fragments::Fragments;
use dicom_core::value::DataSetSequence;
use dicom_core::value::{PixelFragmentSequence, Value};
use dicom_core::{DataElement, PrimitiveValue, VR};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

mod batch;
mod cache;
mod decode_formats;
#[cfg(any(feature = "metal", feature = "cuda"))]
mod device;
mod fixtures;
mod frame_boundaries;
mod frame_io;
mod frame_lifecycle;
mod frame_offsets;
mod image_cache;
mod manifest_building;
mod metadata_parsing;
mod preflight_levels;
mod runtime;
mod twelve_bit;

pub(super) const UNDEFINED_LENGTH_LE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
const JPEG_TRANSFER_SYNTAX: &str = uids::JPEG_BASELINE8_BIT;

fn scan_encapsulated_frames_raw_little_endian(
    path: &Path,
    number_of_frames: u32,
) -> Result<Option<DicomEncapsulatedFrames>, WsiError> {
    scan_encapsulated_frames_raw_little_endian_controlled(path, number_of_frames, None)
        .map(|index| index.map(|index| index.frames))
}

pub(super) fn parse_metadata_object_full(path: &Path) -> Result<ParsedDicomMetadata, WsiError> {
    let budget = OpenBudget::new(SlideLimits::default());
    parse_metadata_object_full_with_budget(path, budget.as_ref())
}

pub(super) fn parse_sparse_tile_map(
    obj: &DefaultDicomObject,
    tile_width: u32,
    tile_height: u32,
) -> Result<HashMap<(u32, u32), u32>, WsiError> {
    let budget = OpenBudget::new(SlideLimits::default());
    parse_sparse_tile_map_with_budget(obj, tile_width, tile_height, budget.as_ref())
}

pub(super) fn preflight_dicom_metadata(
    file: &mut File,
    path: &Path,
) -> Result<DicomPixelDataLocation, WsiError> {
    let budget = OpenBudget::new(SlideLimits::default());
    preflight_dicom_metadata_with_budget(file, path, budget.as_ref())
}

impl DicomSlide {
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        Self::parse_with_cache_config(path, CacheConfig::deterministic())
    }

    pub(super) fn parse_with_cache_config(
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Self, WsiError> {
        Self::parse_with_config(
            path,
            BackendOpenConfig::new(cache_config, SlideLimits::default()),
        )
    }
}

impl DicomImage {
    pub(super) fn read_encapsulated_fragments(
        &self,
        fragments: &[DicomFragmentRef],
    ) -> Result<Vec<u8>, WsiError> {
        frame_index::read_encapsulated_fragments(&self.frame_store.path, fragments)
    }

    pub(super) fn read_encapsulated_frame_group<R: Read + Seek>(
        &self,
        file: &mut R,
        encapsulated_frames: &DicomEncapsulatedFrames,
        group: &DicomFrameReadGroup,
    ) -> Result<Vec<(u32, Vec<u8>)>, WsiError> {
        frame_index::read_encapsulated_frame_group(
            &self.frame_store.path,
            file,
            encapsulated_frames,
            group,
        )
    }
}
