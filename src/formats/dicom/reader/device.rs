use std::borrow::Cow;
use std::sync::Arc;

use j2k_core::BackendRequest;

use crate::core::types::{CpuTile, TileOutputPreference, TilePixels, TileRequest};
use crate::decode::jp2k::{decode_batch_jp2k_pixels, Jp2kDecodeJob};
use crate::decode::jpeg::{decode_batch_jpeg_pixels, JpegDecodeJob};
use crate::error::WsiError;

use super::batch_plan::{
    attach_encapsulated_frame_bytes, check_read_control, DicomBatchPlanMode, DicomBatchPlanner,
    DicomFrameBatchKind, DicomFrameBytes, DicomResolvedBatchFrame, DicomResolvedBatchPlanEntry,
};
use super::DicomReader;
use crate::formats::dicom::backend::{
    dicom_jp2k_device_batch_allowed, dicom_jp2k_device_decode_enabled,
};
use crate::formats::dicom::decode::jp2k_photometric_is_ycbcr;
use crate::formats::dicom::image::DicomImage;
use crate::formats::dicom::JPEG_TRANSFER_SYNTAX;

struct DicomDeviceBatchSelection {
    results: Vec<Option<TilePixels>>,
    job_meta: Vec<DicomResolvedBatchFrame>,
    saw_device_candidate: bool,
}

struct DicomDeviceBatchCompletion {
    results: Vec<Option<TilePixels>>,
    job_meta: Vec<DicomFrameBytes>,
    decoded: Vec<Result<TilePixels, WsiError>>,
}

pub(in super::super) fn complete_mixed_device_batch_with_cpu_remainder(
    reqs: &[TileRequest],
    output: &TileOutputPreference,
    backend: BackendRequest,
    results: Vec<Option<TilePixels>>,
    control: Option<&crate::ReadControl>,
    read_cpu_remainder: impl FnOnce(
        &[TileRequest],
        BackendRequest,
        Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError>,
) -> Result<Vec<TilePixels>, WsiError> {
    check_read_control(control)?;
    let mut results = crate::core::batch::expect_exact_count(
        results,
        reqs.len(),
        "DICOM mixed device batch slots",
    )?;
    let mut remainder_slots = Vec::new();
    let mut remainder_requests = Vec::new();
    for (slot, (result, request)) in results.iter().zip(reqs).enumerate() {
        check_read_control(control)?;
        if result.is_none() {
            remainder_slots.push(slot);
            remainder_requests.push(request.clone());
        }
    }

    check_read_control(control)?;
    if !remainder_requests.is_empty() {
        if output.requires_device() {
            return Err(WsiError::Unsupported {
                reason: "DICOM device batch contained a non-device-decodable tile".into(),
            });
        }
        let cpu_tiles = read_cpu_remainder(&remainder_requests, backend, control)?;
        check_read_control(control)?;
        let cpu_tiles = crate::core::batch::expect_exact_count(
            cpu_tiles,
            remainder_slots.len(),
            "DICOM mixed device CPU remainder batch",
        )?;
        for (slot, tile) in remainder_slots.into_iter().zip(cpu_tiles) {
            results[slot] = Some(TilePixels::Cpu(tile));
        }
    }

    check_read_control(control)?;
    results
        .into_iter()
        .zip(reqs)
        .map(|(tile, req)| {
            tile.ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "DICOM device batch result was not populated".into(),
            })
        })
        .collect()
}

impl DicomReader {
    fn select_device_batch_jobs(
        &self,
        reqs: &[TileRequest],
        output: &TileOutputPreference,
        control: Option<&crate::ReadControl>,
        is_device_decodable: impl Fn(&DicomImage) -> bool,
    ) -> Result<DicomDeviceBatchSelection, WsiError> {
        let mut results: Vec<Option<TilePixels>> = Vec::with_capacity(reqs.len());
        results.resize_with(reqs.len(), || None);
        let mut job_meta = Vec::new();
        let mut saw_device_candidate = false;
        let planner = DicomBatchPlanner::new(
            &self.slide,
            control,
            DicomBatchPlanMode::Device {
                requires_device: output.requires_device(),
            },
        );

        for (slot, req) in reqs.iter().enumerate() {
            match planner.resolve(slot, req, &is_device_decodable)? {
                DicomResolvedBatchPlanEntry::Black(slot, tile) => {
                    results[slot] = Some(TilePixels::Cpu(tile));
                }
                DicomResolvedBatchPlanEntry::CachedFrame(slot, tile) => {
                    saw_device_candidate = true;
                    results[slot] = Some(TilePixels::Cpu(tile));
                }
                DicomResolvedBatchPlanEntry::Frame(meta) => {
                    saw_device_candidate = true;
                    job_meta.push(meta);
                }
                DicomResolvedBatchPlanEntry::Skipped => {}
            }
        }

        Ok(DicomDeviceBatchSelection {
            results,
            job_meta,
            saw_device_candidate,
        })
    }

    fn complete_device_batch_results(
        results: Vec<Option<TilePixels>>,
        no_decodable_frame_reason: &str,
    ) -> Result<Option<Vec<TilePixels>>, WsiError> {
        results
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .map(Some)
            .ok_or_else(|| WsiError::Unsupported {
                reason: no_decodable_frame_reason.into(),
            })
    }

    fn finish_device_batch_results(
        &self,
        reqs: &[TileRequest],
        output: &TileOutputPreference,
        backend: BackendRequest,
        completion: DicomDeviceBatchCompletion,
        control: Option<&crate::ReadControl>,
    ) -> Result<Option<Vec<TilePixels>>, WsiError> {
        let DicomDeviceBatchCompletion {
            mut results,
            job_meta,
            decoded,
        } = completion;
        for ((meta, _), decoded) in job_meta.into_iter().zip(decoded) {
            check_read_control(control)?;
            let tile = decoded?;
            if meta.cache_decoded_frame {
                if let TilePixels::Cpu(cpu) = &tile {
                    meta.image
                        .cache_decoded_frame(meta.frame_index, Arc::new(cpu.clone()));
                }
            }
            results[meta.slot] = Some(tile);
        }

        complete_mixed_device_batch_with_cpu_remainder(
            reqs,
            output,
            backend,
            results,
            control,
            |remainder, backend, control| {
                self.read_tiles_cpu_with_backend_controlled(remainder, backend, control)
            },
        )
        .map(Some)
    }

    pub(super) fn read_tiles_jp2k_device_batch(
        &self,
        reqs: &[TileRequest],
        output: &TileOutputPreference,
        backend: BackendRequest,
        control: Option<&crate::ReadControl>,
    ) -> Result<Option<Vec<TilePixels>>, WsiError> {
        check_read_control(control)?;
        if reqs.is_empty() {
            return Ok(Some(Vec::new()));
        }
        if !output.compressed_device_decode_enabled() && !dicom_jp2k_device_decode_enabled() {
            return Ok(None);
        }
        #[cfg(feature = "metal")]
        let metal_sessions = output.metal_sessions();
        #[cfg(not(feature = "metal"))]
        let metal_sessions = None;
        #[cfg(feature = "cuda")]
        let cuda_sessions = output.cuda_sessions();
        #[cfg(not(feature = "cuda"))]
        let cuda_sessions = None;
        if metal_sessions.is_none() && cuda_sessions.is_none() {
            if output.requires_device() {
                return Err(WsiError::Unsupported {
                    reason:
                        "device backend not available for DICOM JP2K without Metal or CUDA session"
                            .into(),
                });
            }
            return Ok(None);
        }

        let batch = self.select_device_batch_jobs(reqs, output, control, |image| {
            dicom_jp2k_device_batch_allowed(image.transfer_syntax_uid.as_str(), output, reqs.len())
        })?;

        if batch.job_meta.is_empty() && !batch.saw_device_candidate {
            return Ok(None);
        }
        if batch.job_meta.is_empty() {
            return Self::complete_device_batch_results(
                batch.results,
                "DICOM device batch had no decodable JP2K frames",
            );
        }

        let job_meta = attach_encapsulated_frame_bytes(
            batch.job_meta,
            true,
            control,
            DicomFrameBatchKind::Device,
        )?;
        let jobs = job_meta
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
        let decoded = decode_batch_jp2k_pixels(
            &jobs,
            output.requires_device(),
            metal_sessions,
            cuda_sessions,
        );
        check_read_control(control)?;
        if decoded.len() != job_meta.len() {
            return Err(WsiError::Jp2k(format!(
                "DICOM JP2K device batch returned {} tiles for {} jobs",
                decoded.len(),
                job_meta.len()
            )));
        }

        self.finish_device_batch_results(
            reqs,
            output,
            backend,
            DicomDeviceBatchCompletion {
                results: batch.results,
                job_meta,
                decoded,
            },
            control,
        )
    }

    pub(super) fn read_tiles_jpeg_device_batch(
        &self,
        reqs: &[TileRequest],
        output: &TileOutputPreference,
        backend: BackendRequest,
        control: Option<&crate::ReadControl>,
    ) -> Result<Option<Vec<TilePixels>>, WsiError> {
        check_read_control(control)?;
        if reqs.is_empty() {
            return Ok(Some(Vec::new()));
        }
        if !output.compressed_device_decode_enabled() {
            return Ok(None);
        }
        #[cfg(feature = "metal")]
        let metal_sessions = output.metal_sessions();
        #[cfg(not(feature = "metal"))]
        let metal_sessions = None;
        #[cfg(feature = "cuda")]
        let cuda_sessions = output.cuda_sessions();
        #[cfg(not(feature = "cuda"))]
        let cuda_sessions = None;
        if metal_sessions.is_none() && cuda_sessions.is_none() {
            if output.requires_device() {
                return Err(WsiError::Unsupported {
                    reason:
                        "device backend not available for DICOM JPEG without Metal or CUDA session"
                            .into(),
                });
            }
            return Ok(None);
        }

        let batch = self.select_device_batch_jobs(reqs, output, control, |image| {
            image.transfer_syntax_uid == JPEG_TRANSFER_SYNTAX
        })?;

        if batch.job_meta.is_empty() && !batch.saw_device_candidate {
            return Ok(None);
        }
        if batch.job_meta.is_empty() {
            return Self::complete_device_batch_results(
                batch.results,
                "DICOM device batch had no decodable JPEG frames",
            );
        }

        let job_meta = attach_encapsulated_frame_bytes(
            batch.job_meta,
            true,
            control,
            DicomFrameBatchKind::Device,
        )?;
        let jobs = job_meta
            .iter()
            .map(|(meta, bytes)| JpegDecodeJob {
                data: Cow::Borrowed(bytes.as_slice()),
                tables: None,
                expected_width: meta.image.tile_width,
                expected_height: meta.image.tile_height,
                color_transform: j2k_jpeg::ColorTransform::Auto,
                force_dimensions: false,
                requested_size: None,
            })
            .collect::<Vec<_>>();
        check_read_control(control)?;
        let decoded = decode_batch_jpeg_pixels(
            &jobs,
            backend,
            output.requires_device(),
            metal_sessions,
            cuda_sessions,
        );
        check_read_control(control)?;
        if decoded.len() != job_meta.len() {
            return Err(WsiError::Jpeg(format!(
                "DICOM JPEG device batch returned {} tiles for {} jobs",
                decoded.len(),
                job_meta.len()
            )));
        }

        self.finish_device_batch_results(
            reqs,
            output,
            backend,
            DicomDeviceBatchCompletion {
                results: batch.results,
                job_meta,
                decoded,
            },
            control,
        )
    }
}
