use std::borrow::Cow;

use j2k_core::BackendRequest;

use crate::core::types::TileRequest;
use crate::decode::jp2k::Jp2kDecodeJob;
use crate::error::WsiError;

use super::batch_plan::{
    attach_encapsulated_frame_bytes, DicomBatchPlanMode, DicomBatchPlanner, DicomFrameBatchKind,
    DicomFrameBytes, DicomResolvedBatchPlanEntry,
};
use super::DicomReader;
use crate::formats::dicom::decode::jp2k_photometric_is_ycbcr;
use crate::formats::dicom::JP2K_TRANSFER_SYNTAXES;

impl DicomReader {
    pub(super) fn strict_jp2k_frames(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<DicomFrameBytes>, WsiError> {
        if reqs.is_empty() {
            return Ok(Vec::new());
        }
        let planner = DicomBatchPlanner::new(
            &self.slide,
            control,
            DicomBatchPlanMode::Device {
                requires_device: true,
            },
        );
        let mut frames = Vec::with_capacity(reqs.len());
        for (slot, request) in reqs.iter().enumerate() {
            let resolved = planner.resolve(slot, request, |image| {
                JP2K_TRANSFER_SYNTAXES.contains(&image.transfer_syntax_uid.as_str())
            })?;
            match resolved {
                DicomResolvedBatchPlanEntry::Frame(frame) => frames.push(frame),
                DicomResolvedBatchPlanEntry::Skipped => {
                    return Err(WsiError::Unsupported {
                        reason: "strict DICOM device reads support JP2K/HTJ2K frames only".into(),
                    });
                }
                DicomResolvedBatchPlanEntry::Black(..)
                | DicomResolvedBatchPlanEntry::CachedFrame(..) => {
                    return Err(WsiError::Unsupported {
                        reason: "strict DICOM device reads cannot return CPU or synthetic tiles"
                            .into(),
                    });
                }
            }
        }
        let frames =
            attach_encapsulated_frame_bytes(frames, true, control, DicomFrameBatchKind::Device)?;
        crate::core::batch::expect_exact_count(frames, reqs.len(), "DICOM device frame batch")
    }

    #[cfg(feature = "metal")]
    pub(super) fn read_tiles_jp2k_metal(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        let frames = self.strict_jp2k_frames(reqs, None)?;
        self.decode_metal_frames(reqs, &frames, sessions, true)
    }

    #[cfg(feature = "metal")]
    pub(super) fn read_metal_admitted(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::metal::MetalBackendSessions,
        context: &crate::core::limits::ReadExecutionContext<'_>,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        // The ordinary reservation already covers these retained frame inputs.
        // Charge native ownership from their actual sizes, not the conservative
        // encoded-unit ceiling used before the lazy frame index was available.
        let frames = self.strict_jp2k_frames(reqs, context.control)?;
        let copy_bytes = frames.iter().fold(0_u64, |sum, (_, bytes)| {
            sum.saturating_add(bytes.len() as u64)
        });
        let extra = context.try_extra(copy_bytes)?;
        self.decode_metal_frames(reqs, &frames, sessions, extra.is_some())
    }

    #[cfg(feature = "metal")]
    fn decode_metal_frames(
        &self,
        reqs: &[TileRequest],
        frames: &[DicomFrameBytes],
        sessions: &crate::output::metal::MetalBackendSessions,
        grouped: bool,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        let jobs = frames
            .iter()
            .map(|(frame, bytes)| Jp2kDecodeJob {
                data: Cow::Borrowed(bytes.as_slice()),
                expected_width: frame.actual_width,
                expected_height: frame.actual_height,
                rgb_color_space: !jp2k_photometric_is_ycbcr(
                    &frame.image.photometric_interpretation,
                ),
                backend: BackendRequest::Metal,
            })
            .collect::<Vec<_>>();
        let results = if grouped {
            crate::decode::jp2k::decode_batch_jp2k_metal(&jobs, sessions)
        } else {
            jobs.iter()
                .flat_map(|job| {
                    crate::decode::jp2k::decode_batch_jp2k_metal(
                        std::slice::from_ref(job),
                        sessions,
                    )
                })
                .collect()
        };
        results
            .into_iter()
            .zip(reqs)
            .map(|(result, request)| {
                result.map_err(|error| WsiError::TileRead {
                    col: request.col,
                    row: request.row,
                    level: request.level.get(),
                    reason: error.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tiles| {
                crate::core::batch::expect_exact_count(tiles, reqs.len(), "DICOM Metal tile batch")
            })
    }

    #[cfg(feature = "cuda")]
    pub(super) fn read_tiles_jp2k_cuda(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        let frames = self.strict_jp2k_frames(reqs, None)?;
        let jobs = frames
            .iter()
            .map(|(frame, bytes)| Jp2kDecodeJob {
                data: Cow::Borrowed(bytes.as_slice()),
                expected_width: frame.actual_width,
                expected_height: frame.actual_height,
                rgb_color_space: !jp2k_photometric_is_ycbcr(
                    &frame.image.photometric_interpretation,
                ),
                backend: BackendRequest::Cuda,
            })
            .collect::<Vec<_>>();
        crate::decode::jp2k::decode_batch_jp2k_cuda(&jobs, sessions)
            .into_iter()
            .zip(reqs)
            .map(|(result, request)| {
                result.map_err(|error| WsiError::TileRead {
                    col: request.col,
                    row: request.row,
                    level: request.level.get(),
                    reason: error.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tiles| {
                crate::core::batch::expect_exact_count(tiles, reqs.len(), "DICOM CUDA tile batch")
            })
    }
}
