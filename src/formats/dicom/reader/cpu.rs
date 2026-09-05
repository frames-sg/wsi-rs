use std::borrow::Cow;
use std::sync::Arc;

use j2k_core::BackendRequest;

use crate::core::types::{CpuTile, TileRequest};
use crate::decode::jp2k::{decode_batch_jp2k, Jp2kDecodeJob};
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::error::WsiError;

use super::batch_plan::{
    attach_encapsulated_frame_bytes, check_read_control, DicomBatchPlanMode, DicomBatchPlanner,
    DicomFrameBatchKind, DicomResolvedBatchPlanEntry,
};
use super::DicomReader;
use crate::formats::dicom::decode::{
    crop_or_keep_sample_buffer_rgb, decode_rle_lossless_frame, dicom_jpeg_color_transform,
    jp2k_photometric_is_ycbcr, validate_jpeg_transfer_syntax_frame,
};
use crate::formats::dicom::{is_jpeg_transfer_syntax, JP2K_TRANSFER_SYNTAXES, RLE_TRANSFER_SYNTAX};

impl DicomReader {
    pub(super) fn read_tiles_cpu_with_backend_controlled(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        check_read_control(control)?;
        if reqs.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = vec![None; reqs.len()];
        let mut jpeg_metas = Vec::new();
        let mut jp2k_metas = Vec::new();
        let mut rle_metas = Vec::new();
        let planner = DicomBatchPlanner::new(&self.slide, control, DicomBatchPlanMode::Cpu);

        for (slot, req) in reqs.iter().enumerate() {
            match planner.resolve(slot, req, |_| true)? {
                DicomResolvedBatchPlanEntry::Black(slot, tile)
                | DicomResolvedBatchPlanEntry::CachedFrame(slot, tile) => {
                    results[slot] = Some(tile);
                }
                DicomResolvedBatchPlanEntry::Frame(meta) => {
                    if is_jpeg_transfer_syntax(&meta.image.transfer_syntax_uid) {
                        jpeg_metas.push(meta);
                    } else if JP2K_TRANSFER_SYNTAXES
                        .contains(&meta.image.transfer_syntax_uid.as_str())
                    {
                        jp2k_metas.push(meta);
                    } else if meta.image.transfer_syntax_uid == RLE_TRANSFER_SYNTAX {
                        rle_metas.push(meta);
                    } else {
                        results[slot] = Some(self.read_tile_with_backend(req, backend)?);
                    }
                }
                #[cfg(any(feature = "metal", feature = "cuda"))]
                DicomResolvedBatchPlanEntry::Skipped => {
                    unreachable!("CPU DICOM batch planning does not skip resolved requests")
                }
            }
        }

        let jpeg_jobs =
            attach_encapsulated_frame_bytes(jpeg_metas, false, control, DicomFrameBatchKind::Cpu)?;
        let jpeg_decode_jobs = jpeg_jobs
            .iter()
            .map(|(meta, bytes)| {
                validate_jpeg_transfer_syntax_frame(
                    &meta.image.transfer_syntax_uid,
                    bytes.as_slice(),
                )
                .map_err(|err| WsiError::TileRead {
                    col: meta.req.col,
                    row: meta.req.row,
                    level: meta.req.level.get(),
                    reason: err.to_string(),
                })?;
                Ok(JpegDecodeJob {
                    data: Cow::Borrowed(bytes.as_slice()),
                    tables: None,
                    expected_width: meta.image.tile_width,
                    expected_height: meta.image.tile_height,
                    color_transform: dicom_jpeg_color_transform(
                        &meta.image.photometric_interpretation,
                    ),
                    force_dimensions: false,
                    requested_size: None,
                })
            })
            .collect::<Result<Vec<_>, WsiError>>()?;
        check_read_control(control)?;
        let jpeg_decoded = crate::core::batch::expect_exact_count(
            decode_batch_jpeg(&jpeg_decode_jobs),
            jpeg_decode_jobs.len(),
            "DICOM JPEG batch decode",
        )?;
        check_read_control(control)?;
        for ((meta, _), decoded) in jpeg_jobs.into_iter().zip(jpeg_decoded) {
            let tile = decoded.map_err(|err| WsiError::TileRead {
                col: meta.req.col,
                row: meta.req.row,
                level: meta.req.level.get(),
                reason: err.to_string(),
            })?;
            if meta.cache_decoded_frame {
                meta.image
                    .cache_decoded_frame(meta.frame_index, Arc::new(tile.clone()));
            }
            results[meta.slot] = Some(crop_or_keep_sample_buffer_rgb(
                tile,
                meta.actual_width,
                meta.actual_height,
            )?);
        }

        let jp2k_jobs =
            attach_encapsulated_frame_bytes(jp2k_metas, false, control, DicomFrameBatchKind::Cpu)?;
        let jp2k_decode_jobs = jp2k_jobs
            .iter()
            .map(|(meta, bytes)| Jp2kDecodeJob {
                data: Cow::Borrowed(bytes.as_slice()),
                expected_width: meta.image.tile_width,
                expected_height: meta.image.tile_height,
                rgb_color_space: !jp2k_photometric_is_ycbcr(
                    meta.image.photometric_interpretation.as_str(),
                ),
                backend,
            })
            .collect::<Vec<_>>();
        check_read_control(control)?;
        let jp2k_decoded = crate::core::batch::expect_exact_count(
            decode_batch_jp2k(&jp2k_decode_jobs),
            jp2k_decode_jobs.len(),
            "DICOM JP2K batch decode",
        )?;
        check_read_control(control)?;
        for ((meta, _), decoded) in jp2k_jobs.into_iter().zip(jp2k_decoded) {
            let tile = decoded.map_err(|err| WsiError::TileRead {
                col: meta.req.col,
                row: meta.req.row,
                level: meta.req.level.get(),
                reason: err.to_string(),
            })?;
            if meta.cache_decoded_frame {
                meta.image
                    .cache_decoded_frame(meta.frame_index, Arc::new(tile.clone()));
            }
            results[meta.slot] = Some(crop_or_keep_sample_buffer_rgb(
                tile,
                meta.actual_width,
                meta.actual_height,
            )?);
        }

        let rle_jobs =
            attach_encapsulated_frame_bytes(rle_metas, false, control, DicomFrameBatchKind::Cpu)?;
        for (meta, bytes) in rle_jobs {
            check_read_control(control)?;
            let tile = decode_rle_lossless_frame(
                bytes.as_slice(),
                meta.image.tile_width,
                meta.image.tile_height,
                meta.image.samples_per_pixel,
                &meta.image.photometric_interpretation,
            )
            .map_err(|err| WsiError::TileRead {
                col: meta.req.col,
                row: meta.req.row,
                level: meta.req.level.get(),
                reason: err.to_string(),
            })?;
            if meta.cache_decoded_frame {
                meta.image
                    .cache_decoded_frame(meta.frame_index, Arc::new(tile.clone()));
            }
            results[meta.slot] = Some(crop_or_keep_sample_buffer_rgb(
                tile,
                meta.actual_width,
                meta.actual_height,
            )?);
        }

        check_read_control(control)?;
        results
            .into_iter()
            .zip(reqs.iter())
            .map(|(tile, req)| {
                tile.ok_or_else(|| WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "DICOM CPU batch result was not populated".into(),
                })
            })
            .collect()
    }

    pub(super) fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let image =
            self.slide
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                })?;
        image.read_tile(req.col, req.row, req.level.get(), backend)
    }
}
