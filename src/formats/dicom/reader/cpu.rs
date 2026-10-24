use std::collections::HashMap;
use std::sync::Arc;

use j2k_core::BackendRequest;

use crate::core::read_control::check_cancelled_opt;
use crate::core::types::{CpuTile, TileRequest};
use crate::decode::jp2k::decode_batch_jp2k;
use crate::error::WsiError;

use super::batch_plan::{
    attach_encapsulated_frame_bytes, DicomBatchPlanMode, DicomBatchPlanner, DicomFrameBatchKind,
    DicomFrameBytes, DicomResolvedBatchFrame, DicomResolvedBatchPlanEntry,
};
use super::{dicom_level_for_request, DicomReader};
use crate::formats::dicom::decode::{crop_or_keep_sample_buffer_rgb, decode_dicom_jpeg_batch};
use crate::formats::dicom::metadata::DicomBitDepth;
use crate::formats::dicom::transfer_syntax::DicomTransferSyntax;

impl DicomReader {
    pub(super) fn read_tiles_cpu_with_backend_controlled(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        check_cancelled_opt(control)?;
        if reqs.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = vec![None; reqs.len()];
        let mut jpeg_metas = Vec::new();
        let mut jp2k_metas = Vec::new();
        let mut rle_metas = Vec::new();
        let mut first_frame_slots = HashMap::new();
        let mut duplicate_frame_slots = Vec::new();
        let deduplicate_frames = reqs.len() > 1;
        let planner = DicomBatchPlanner::new(&self.slide, control, DicomBatchPlanMode::Cpu);

        for (slot, req) in reqs.iter().enumerate() {
            match planner.resolve(slot, req, |_| true)? {
                DicomResolvedBatchPlanEntry::Black(slot, tile)
                | DicomResolvedBatchPlanEntry::CachedFrame(slot, tile) => {
                    results[slot] = Some(tile);
                }
                DicomResolvedBatchPlanEntry::Frame(meta) => {
                    if deduplicate_frames {
                        let key = (
                            req.scene.get(),
                            req.series.get(),
                            req.level.get(),
                            req.plane.get(),
                            req.col,
                            req.row,
                        );
                        if let Some(&first_slot) = first_frame_slots.get(&key) {
                            duplicate_frame_slots.push((slot, first_slot));
                            continue;
                        }
                        first_frame_slots.insert(key, slot);
                    }
                    match meta.image.transfer_syntax {
                        DicomTransferSyntax::Jpeg(_) => jpeg_metas.push(meta),
                        DicomTransferSyntax::Jp2k { .. } => jp2k_metas.push(meta),
                        DicomTransferSyntax::Rle => rle_metas.push(meta),
                        DicomTransferSyntax::Native { .. } => {
                            results[slot] = Some(self.read_tile_with_backend(req, backend)?);
                        }
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
        // Pyramid levels share one precision, so the whole batch has one output type.
        if let Some(bit_depth) = jpeg_jobs.first().map(|(meta, _)| meta.image.bit_depth) {
            decode_jpeg_frames_into(bit_depth, jpeg_jobs, control, &mut results)?;
        }

        let jp2k_jobs =
            attach_encapsulated_frame_bytes(jp2k_metas, false, control, DicomFrameBatchKind::Cpu)?;
        check_cancelled_opt(control)?;
        let jp2k_decoded = if jp2k_jobs
            .first()
            .is_some_and(|(meta, _)| matches!(meta.image.bit_depth, DicomBitDepth::Uint16 { .. }))
        {
            crate::core::batch::install_codec_cpu(|| {
                use rayon::prelude::*;
                jp2k_jobs
                    .par_iter()
                    .map(|(meta, bytes)| {
                        meta.image.decode_jp2k_u16_frame(
                            bytes.as_slice(),
                            j2k::CpuDecodeParallelism::Serial,
                        )
                    })
                    .collect::<Vec<_>>()
            })
        } else {
            let jp2k_decode_jobs = jp2k_jobs
                .iter()
                .map(|(meta, bytes)| meta.image.jp2k_job(bytes.as_slice(), backend))
                .collect::<Vec<_>>();
            decode_batch_jp2k(&jp2k_decode_jobs)
        };
        let jp2k_decoded = crate::core::batch::expect_exact_count(
            jp2k_decoded,
            jp2k_jobs.len(),
            "DICOM JP2K batch decode",
        )?;
        check_cancelled_opt(control)?;
        for ((meta, _), decoded) in jp2k_jobs.into_iter().zip(jp2k_decoded) {
            store_decoded(meta, decoded, &mut results)?;
        }

        let rle_jobs =
            attach_encapsulated_frame_bytes(rle_metas, false, control, DicomFrameBatchKind::Cpu)?;
        for (meta, bytes) in rle_jobs {
            check_cancelled_opt(control)?;
            let decoded = meta.image.decode_rle(bytes.as_slice());
            store_decoded(meta, decoded, &mut results)?;
        }

        for (slot, first_slot) in duplicate_frame_slots {
            results[slot] = results[first_slot].clone();
        }

        check_cancelled_opt(control)?;
        results
            .into_iter()
            .zip(reqs.iter())
            .map(|(tile, req)| {
                tile.ok_or_else(|| {
                    WsiError::tile_read(req, "DICOM CPU batch result was not populated")
                })
            })
            .collect()
    }

    pub(super) fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let image = dicom_level_for_request(&self.slide, req)?;
        image.read_tile(req.col, req.row, req.level.get(), backend)
    }
}

/// Decode JPEG frames of one precision and store the cropped tiles in their
/// request slots.
fn decode_jpeg_frames_into(
    bit_depth: DicomBitDepth,
    jpeg_jobs: Vec<DicomFrameBytes>,
    control: Option<&crate::ReadControl>,
    results: &mut [Option<CpuTile>],
) -> Result<(), WsiError> {
    let jpeg_decode_jobs = jpeg_jobs
        .iter()
        .map(|(meta, bytes)| {
            meta.image
                .jpeg_job(bytes.as_slice())
                .map_err(|err| WsiError::tile_read(&meta.req, err.to_string()))
        })
        .collect::<Result<Vec<_>, WsiError>>()?;
    check_cancelled_opt(control)?;
    let jpeg_decoded = crate::core::batch::expect_exact_count(
        decode_dicom_jpeg_batch(bit_depth, &jpeg_decode_jobs),
        jpeg_decode_jobs.len(),
        "DICOM JPEG batch decode",
    )?;
    check_cancelled_opt(control)?;
    for ((meta, _), decoded) in jpeg_jobs.into_iter().zip(jpeg_decoded) {
        store_decoded(meta, decoded, results)?;
    }
    Ok(())
}

/// Store a decoded batch frame, cropped to its tile, in its request slot,
/// caching the full frame when the batch allows it.
fn store_decoded(
    meta: DicomResolvedBatchFrame,
    decoded: Result<CpuTile, WsiError>,
    results: &mut [Option<CpuTile>],
) -> Result<(), WsiError> {
    let tile = decoded.map_err(|err| WsiError::tile_read(&meta.req, err.to_string()))?;
    if meta.cache_decoded_frame {
        meta.image
            .cache_decoded_frame(meta.frame_index, Arc::new(tile.clone()));
    }
    results[meta.slot] = Some(crop_or_keep_sample_buffer_rgb(
        tile,
        meta.actual_width,
        meta.actual_height,
    )?);
    Ok(())
}
