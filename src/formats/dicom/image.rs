use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use j2k_core::BackendRequest;

use crate::core::cache::{PrivateCache, PrivateCacheBudget};
use crate::core::file_identity::FileIdentity;
use crate::core::types::{CpuTile, RawCompressedTile};
use crate::error::WsiError;

use super::backend::is_encapsulated_transfer_syntax;
use super::decode::{
    black_sample_buffer, checked_dicom_tile_coordinates, crop_or_keep_sample_buffer_rgb,
    dicom_actual_tile_dimensions, raw_compression_for_transfer_syntax,
    raw_photometric_interpretation, trim_encapsulated_frame_padding,
};
use super::frame_index::DicomEncapsulatedFrames;
use super::metadata::{invalid_slide, parse_sparse_tile_map, ParsedDicomMetadata};
use super::preflight::DicomPixelDataLocation;

mod frame_decode;
mod frame_io;

pub(super) const BATCH_FRAME_READ_MAX_SPAN_BYTES: u64 = 32 * 1024 * 1024;
pub(super) const BATCH_FRAME_READ_MAX_GAP_BYTES: u64 = 64 * 1024;

#[derive(Debug)]
pub(super) struct DicomImage {
    pub(super) sop_instance_uid: String,
    pub(super) transfer_syntax_uid: String,
    pub(super) photometric_interpretation: String,
    pub(super) samples_per_pixel: u16,
    pub(super) planar_configuration: Option<u16>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
    pub(super) tiles_across: u32,
    pub(super) tiles_down: u32,
    pub(super) number_of_frames: u32,
    pub(super) grid: DicomGrid,
    pub(super) pixel_spacing: Option<(f64, f64)>,
    pub(super) objective_lens_power: Option<f64>,
    pub(super) frame_store: DicomFrameStore,
    pub(super) decoded_frame_cache: Mutex<PrivateCache<u32, Arc<CpuTile>>>,
}

#[derive(Debug)]
pub(super) struct DicomFrameStore {
    pub(super) path: PathBuf,
    pub(super) native_pixel_data: Option<NativePixelData>,
    pub(super) encapsulated_frames: Mutex<Option<Arc<DicomEncapsulatedFrames>>>,
    pub(super) compressed_frame_cache: Mutex<PrivateCache<u32, Arc<Vec<u8>>>>,
}

#[derive(Debug)]
pub(super) struct NativePixelData {
    source_identity: FileIdentity,
    value_offset: u64,
    value_len: u32,
}

#[derive(Debug)]
pub(super) enum DicomGrid {
    Full,
    Sparse(HashMap<(u32, u32), u32>),
}

impl DicomImage {
    pub(super) fn from_metadata_with_private_cache_budget(
        meta: ParsedDicomMetadata,
        private_cache_budget: &mut PrivateCacheBudget,
    ) -> Result<Self, WsiError> {
        let width = meta.total_pixel_matrix_columns.unwrap_or(meta.columns);
        let height = meta.total_pixel_matrix_rows.unwrap_or(meta.rows);
        let tile_width = meta.columns;
        let tile_height = meta.rows;
        let tiles_across = width.div_ceil(tile_width);
        let tiles_down = height.div_ceil(tile_height);
        let grid = if meta.dimension_organization_type.as_deref() == Some("TILED_SPARSE") {
            DicomGrid::Sparse(parse_sparse_tile_map(&meta.obj, tile_width, tile_height)?)
        } else {
            DicomGrid::Full
        };
        let estimated_frame_bytes =
            dicom_frame_cache_entry_bytes(tile_width, tile_height, meta.samples_per_pixel);
        let encapsulated_frame_cache =
            PrivateCache::new(private_cache_budget.allocate(estimated_frame_bytes));
        let decoded_frame_cache =
            PrivateCache::new(private_cache_budget.allocate(estimated_frame_bytes));
        let native_pixel_data = if is_encapsulated_transfer_syntax(&meta.transfer_syntax_uid) {
            if meta.pixel_data != DicomPixelDataLocation::Encapsulated {
                return Err(invalid_slide(
                    &meta.path,
                    "encapsulated transfer syntax does not have encapsulated Pixel Data",
                ));
            }
            None
        } else {
            let DicomPixelDataLocation::Native {
                value_offset,
                value_len,
            } = meta.pixel_data
            else {
                return Err(invalid_slide(
                    &meta.path,
                    "native transfer syntax does not have native Pixel Data",
                ));
            };
            Some(NativePixelData {
                source_identity: meta.source_identity,
                value_offset,
                value_len,
            })
        };
        Ok(Self {
            sop_instance_uid: meta.sop_instance_uid,
            transfer_syntax_uid: meta.transfer_syntax_uid,
            photometric_interpretation: meta.photometric_interpretation,
            samples_per_pixel: meta.samples_per_pixel,
            planar_configuration: meta.planar_configuration,
            width,
            height,
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
            number_of_frames: meta.number_of_frames,
            grid,
            pixel_spacing: meta.pixel_spacing,
            objective_lens_power: meta.objective_lens_power,
            frame_store: DicomFrameStore {
                path: meta.path,
                native_pixel_data,
                encapsulated_frames: Mutex::new(None),
                compressed_frame_cache: Mutex::new(encapsulated_frame_cache),
            },
            decoded_frame_cache: Mutex::new(decoded_frame_cache),
        })
    }

    pub(super) fn read_tile(
        &self,
        col: i64,
        row: i64,
        level: u32,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let span = tracing::info_span!(
            "dicom_read_tile",
            reader = "wsi_rs",
            transfer_syntax = %self.transfer_syntax_uid,
        );
        let _guard = span.enter();
        let (col_u32, row_u32) =
            checked_dicom_tile_coordinates(col, row, level, self.tiles_across, self.tiles_down)?;
        let Some(frame_index) = self.frame_index(col_u32, row_u32) else {
            let (width, height) = self.actual_tile_dimensions(col_u32, row_u32);
            return black_sample_buffer(width, height);
        };

        let (actual_width, actual_height) = self.actual_tile_dimensions(col_u32, row_u32);
        let buffer = self.decode_frame_sample_buffer(frame_index, level, col, row, backend)?;
        crop_or_keep_sample_buffer_rgb(buffer, actual_width, actual_height)
    }

    pub(super) fn read_raw_compressed_tile(
        &self,
        col: i64,
        row: i64,
        level: u32,
    ) -> Result<RawCompressedTile, WsiError> {
        let (col_u32, row_u32) =
            checked_dicom_tile_coordinates(col, row, level, self.tiles_across, self.tiles_down)?;
        let Some(frame_index) = self.frame_index(col_u32, row_u32) else {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "raw compressed tile access is not available for sparse missing DICOM tile ({col}, {row}) at level {level}"
                ),
            });
        };
        let compression = raw_compression_for_transfer_syntax(
            &self.transfer_syntax_uid,
            &self.photometric_interpretation,
        )?;
        let photometric_interpretation = raw_photometric_interpretation(
            self.samples_per_pixel,
            &self.photometric_interpretation,
        )?;
        let bytes = self.extract_encapsulated_frame(frame_index, level, col, row, true)?;
        let mut data = bytes.as_ref().clone();
        trim_encapsulated_frame_padding(&mut data);

        Ok(RawCompressedTile::builder(compression)
            .dimensions(self.tile_width, self.tile_height)
            .bits_allocated(8)
            .samples_per_pixel(self.samples_per_pixel)
            .photometric_interpretation(photometric_interpretation)
            .data(data)
            .build()?)
    }

    pub(super) fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let buffer = self
            .decode_frame_sample_buffer(0, 0, 0, 0, BackendRequest::Auto)
            .map_err(|err| match err {
                WsiError::TileRead { reason, .. } => {
                    WsiError::AssociatedImageNotFound(format!("{name}: {reason}"))
                }
                other => other,
            })?;
        crop_or_keep_sample_buffer_rgb(buffer, self.width, self.height)
    }

    pub(super) fn frame_index(&self, col: u32, row: u32) -> Option<u32> {
        match &self.grid {
            DicomGrid::Full => Some(row * self.tiles_across + col),
            DicomGrid::Sparse(map) => map.get(&(col, row)).copied(),
        }
    }

    pub(super) fn is_full_grid(&self) -> bool {
        matches!(self.grid, DicomGrid::Full)
    }

    pub(super) fn actual_tile_dimensions(&self, col: u32, row: u32) -> (u32, u32) {
        dicom_actual_tile_dimensions(
            self.width,
            self.height,
            self.tile_width,
            self.tile_height,
            col,
            row,
        )
    }

    pub(super) fn cached_decoded_frame(&self, frame_index: u32) -> Option<Arc<CpuTile>> {
        self.decoded_frame_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&frame_index)
            .cloned()
    }

    pub(super) fn cache_decoded_frame(&self, frame_index: u32, tile: Arc<CpuTile>) {
        self.decoded_frame_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(frame_index, tile);
    }

    pub(super) fn should_cache_decoded_frames_for_batch(&self, batch_len: usize) -> bool {
        batch_len
            <= self
                .decoded_frame_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .capacity_entries()
    }
}

pub(super) fn dicom_frame_cache_entry_bytes(
    tile_width: u32,
    tile_height: u32,
    samples_per_pixel: u16,
) -> u64 {
    u64::from(tile_width)
        .saturating_mul(u64::from(tile_height))
        .saturating_mul(u64::from(samples_per_pixel))
        // Keep the estimate safe for both the supported 8-bit transfer
        // syntaxes and future 16-bit decoded storage.
        .saturating_mul(2)
}
