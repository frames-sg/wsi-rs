use std::borrow::Cow;
use std::fs::File;
use std::sync::Arc;

use j2k_core::BackendRequest;

use crate::core::file_identity::FileIdentity;
use crate::core::types::CpuTile;
use crate::decode::jp2k::{decode_batch_jp2k, Jp2kDecodeJob};
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::error::WsiError;

use super::DicomImage;
use crate::formats::dicom::backend::{is_encapsulated_transfer_syntax, is_jpeg_transfer_syntax};
use crate::formats::dicom::decode::{
    decode_rle_lossless_frame, dicom_jpeg_color_transform, frame_bytes_to_rgb_tile,
    jp2k_photometric_is_ycbcr, validate_jpeg_transfer_syntax_frame,
};
use crate::formats::dicom::frame_index::read_exact_at;
use crate::formats::dicom::metadata::invalid_slide;
use crate::formats::dicom::{JP2K_TRANSFER_SYNTAXES, RLE_TRANSFER_SYNTAX};

impl DicomImage {
    pub(in super::super) fn decode_uncompressed_frame_sample_buffer(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
    ) -> Result<CpuTile, WsiError> {
        let frame_len = crate::core::limits::checked_product_to_usize(
            &[
                u64::from(self.tile_width),
                u64::from(self.tile_height),
                u64::from(self.samples_per_pixel),
            ],
            crate::core::limits::MAX_DECODED_IMAGE_BYTES,
            "native DICOM frame",
        )
        .map_err(|reason| WsiError::TileRead {
            col,
            row,
            level,
            reason,
        })?;
        let start = u64::from(frame_index)
            .checked_mul(frame_len as u64)
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: "DICOM frame offset overflow".into(),
            })?;
        let end = start
            .checked_add(frame_len as u64)
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: "DICOM frame byte range overflow".into(),
            })?;
        let native_pixel_data =
            self.frame_store
                .native_pixel_data
                .as_ref()
                .ok_or_else(|| WsiError::TileRead {
                    col,
                    row,
                    level,
                    reason: "native DICOM Pixel Data location is unavailable".into(),
                })?;
        if end > u64::from(native_pixel_data.value_len) {
            return Err(WsiError::TileRead {
                col,
                row,
                level,
                reason: format!(
                    "DICOM frame {frame_index} byte range {}..{} exceeds pixel data length {}",
                    start, end, native_pixel_data.value_len
                ),
            });
        }
        let absolute_start = native_pixel_data
            .value_offset
            .checked_add(start)
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: "DICOM frame file offset overflow".into(),
            })?;
        let mut frame = Vec::new();
        frame
            .try_reserve_exact(frame_len)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "native DICOM frame",
                requested: frame_len as u64,
                limit: crate::core::limits::MAX_DECODED_IMAGE_BYTES,
            })?;
        frame.resize(frame_len, 0);
        let mut file =
            File::open(&self.frame_store.path).map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: self.frame_store.path.clone(),
            })?;
        if FileIdentity::from_open_file(&self.frame_store.path, &file)?
            != native_pixel_data.source_identity
        {
            return Err(invalid_slide(
                &self.frame_store.path,
                "DICOM source changed after metadata was parsed",
            ));
        }
        read_exact_at(
            &mut file,
            &self.frame_store.path,
            absolute_start,
            &mut frame,
        )?;
        frame_bytes_to_rgb_tile(
            &frame,
            self.tile_width,
            self.tile_height,
            self.samples_per_pixel,
            self.planar_configuration.unwrap_or(0),
            &self.photometric_interpretation,
        )
        .map_err(|err| WsiError::TileRead {
            col,
            row,
            level,
            reason: err.to_string(),
        })
    }

    pub(in super::super) fn decode_frame_sample_buffer(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let use_decoded_cache = is_encapsulated_transfer_syntax(&self.transfer_syntax_uid);
        if use_decoded_cache {
            if let Some(cached) = self.cached_decoded_frame(frame_index) {
                return Ok(cached.as_ref().clone());
            }
        }

        let buffer = if is_jpeg_transfer_syntax(&self.transfer_syntax_uid) {
            let bytes =
                self.extract_encapsulated_frame(frame_index, level, col, row, !use_decoded_cache)?;
            validate_jpeg_transfer_syntax_frame(&self.transfer_syntax_uid, bytes.as_slice())
                .map_err(|err| WsiError::TileRead {
                    col,
                    row,
                    level,
                    reason: err.to_string(),
                })?;
            crate::core::batch::exactly_one(
                decode_batch_jpeg(&[JpegDecodeJob {
                    data: Cow::Borrowed(bytes.as_slice()),
                    tables: None,
                    expected_width: self.tile_width,
                    expected_height: self.tile_height,
                    color_transform: dicom_jpeg_color_transform(&self.photometric_interpretation),
                    force_dimensions: false,
                    requested_size: None,
                }]),
                "DICOM JPEG frame decode",
            )?
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: err.to_string(),
            })?
        } else if JP2K_TRANSFER_SYNTAXES.contains(&self.transfer_syntax_uid.as_str()) {
            let bytes =
                self.extract_encapsulated_frame(frame_index, level, col, row, !use_decoded_cache)?;
            crate::core::batch::exactly_one(
                decode_batch_jp2k(&[Jp2kDecodeJob {
                    data: Cow::Borrowed(bytes.as_slice()),
                    expected_width: self.tile_width,
                    expected_height: self.tile_height,
                    rgb_color_space: !jp2k_photometric_is_ycbcr(
                        self.photometric_interpretation.as_str(),
                    ),
                    backend,
                }]),
                "DICOM JP2K frame decode",
            )?
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: err.to_string(),
            })?
        } else if self.transfer_syntax_uid == RLE_TRANSFER_SYNTAX {
            let bytes =
                self.extract_encapsulated_frame(frame_index, level, col, row, !use_decoded_cache)?;
            decode_rle_lossless_frame(
                bytes.as_slice(),
                self.tile_width,
                self.tile_height,
                self.samples_per_pixel,
                &self.photometric_interpretation,
            )
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: err.to_string(),
            })?
        } else {
            self.decode_uncompressed_frame_sample_buffer(frame_index, level, col, row)?
        };

        if use_decoded_cache {
            self.cache_decoded_frame(frame_index, Arc::new(buffer.clone()));
        }
        Ok(buffer)
    }
}
