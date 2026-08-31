use std::collections::HashMap;
use std::sync::Arc;

use crate::core::types::{CpuTile, TileRequest};
use crate::error::WsiError;

use crate::formats::dicom::backend::is_encapsulated_transfer_syntax;
use crate::formats::dicom::decode::{black_sample_buffer, crop_sample_buffer_rgb};
use crate::formats::dicom::image::DicomImage;
use crate::formats::dicom::manifest::DicomSlide;

#[derive(Clone)]
pub(super) struct DicomResolvedBatchFrame {
    pub(super) slot: usize,
    pub(super) req: TileRequest,
    pub(super) image: Arc<DicomImage>,
    pub(super) frame_index: u32,
    pub(super) actual_width: u32,
    pub(super) actual_height: u32,
    pub(super) cache_decoded_frame: bool,
}

pub(super) enum DicomResolvedBatchPlanEntry {
    Black(usize, CpuTile),
    CachedFrame(usize, CpuTile),
    Frame(DicomResolvedBatchFrame),
    #[cfg(any(feature = "metal", feature = "cuda"))]
    Skipped,
}

#[derive(Clone, Copy)]
pub(super) enum DicomBatchPlanMode {
    Cpu,
    #[cfg(any(feature = "metal", feature = "cuda"))]
    Device {
        requires_device: bool,
    },
}

pub(super) type DicomFrameBytes = (DicomResolvedBatchFrame, Arc<Vec<u8>>);

#[derive(Clone, Copy)]
pub(super) enum DicomFrameBatchKind {
    Cpu,
    #[cfg(any(feature = "metal", feature = "cuda"))]
    Device,
}

pub(super) fn check_read_control(control: Option<&crate::ReadControl>) -> Result<(), WsiError> {
    control.map_or(Ok(()), crate::ReadControl::check_cancelled)
}

pub(super) struct DicomBatchPlanner<'a> {
    slide: &'a DicomSlide,
    control: Option<&'a crate::ReadControl>,
    mode: DicomBatchPlanMode,
}

impl<'a> DicomBatchPlanner<'a> {
    pub(super) fn new(
        slide: &'a DicomSlide,
        control: Option<&'a crate::ReadControl>,
        mode: DicomBatchPlanMode,
    ) -> Self {
        Self {
            slide,
            control,
            mode,
        }
    }

    pub(super) fn resolve(
        &self,
        slot: usize,
        req: &TileRequest,
        _is_device_decodable: impl FnOnce(&DicomImage) -> bool,
    ) -> Result<DicomResolvedBatchPlanEntry, WsiError> {
        check_read_control(self.control)?;
        let level =
            self.slide
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                })?;
        if req.col < 0
            || req.row < 0
            || req.col >= level.tiles_across as i64
            || req.row >= level.tiles_down as i64
        {
            return Err(match self.mode {
                DicomBatchPlanMode::Cpu => WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: format!(
                        "tile ({},{}) out of range ({}x{})",
                        req.col, req.row, level.tiles_across, level.tiles_down
                    ),
                },
                #[cfg(any(feature = "metal", feature = "cuda"))]
                DicomBatchPlanMode::Device { .. } => WsiError::Unsupported {
                    reason: format!(
                        "tile ({},{}) out of range for DICOM device decode",
                        req.col, req.row
                    ),
                },
            });
        }

        let col = req.col as u32;
        let row = req.row as u32;
        let black_tile = || {
            #[cfg(any(feature = "metal", feature = "cuda"))]
            if matches!(
                self.mode,
                DicomBatchPlanMode::Device {
                    requires_device: true
                }
            ) {
                return Err(WsiError::Unsupported {
                    reason:
                        "DICOM device batch cannot return CPU black tile for sparse missing tile"
                            .into(),
                });
            }
            let (width, height) = level.actual_tile_dimensions(col, row);
            Ok(DicomResolvedBatchPlanEntry::Black(
                slot,
                black_sample_buffer(width, height)?,
            ))
        };
        let Some(image) = level.image_for_tile(col, row) else {
            return black_tile();
        };

        #[cfg(any(feature = "metal", feature = "cuda"))]
        if matches!(self.mode, DicomBatchPlanMode::Device { .. })
            && !_is_device_decodable(image.as_ref())
        {
            return Ok(DicomResolvedBatchPlanEntry::Skipped);
        }

        let Some(frame_index) = image.frame_index(col, row) else {
            return black_tile();
        };
        let (actual_width, actual_height) = level.actual_tile_dimensions(col, row);

        match self.mode {
            DicomBatchPlanMode::Cpu => {
                if is_encapsulated_transfer_syntax(&image.transfer_syntax_uid) {
                    if let Some(cached) = image.cached_decoded_frame(frame_index) {
                        return Ok(DicomResolvedBatchPlanEntry::CachedFrame(
                            slot,
                            crop_sample_buffer_rgb(cached.as_ref(), actual_width, actual_height)?,
                        ));
                    }
                }
            }
            #[cfg(any(feature = "metal", feature = "cuda"))]
            DicomBatchPlanMode::Device { requires_device } => {
                if image.samples_per_pixel != 3 {
                    return Ok(DicomResolvedBatchPlanEntry::Skipped);
                }
                if !requires_device {
                    if let Some(cached) = image.cached_decoded_frame(frame_index) {
                        return Ok(DicomResolvedBatchPlanEntry::CachedFrame(
                            slot,
                            cached.as_ref().clone(),
                        ));
                    }
                }
            }
        }

        Ok(DicomResolvedBatchPlanEntry::Frame(
            DicomResolvedBatchFrame {
                slot,
                req: req.clone(),
                image,
                frame_index,
                actual_width,
                actual_height,
                cache_decoded_frame: true,
            },
        ))
    }
}

pub(super) fn attach_encapsulated_frame_bytes(
    metas: Vec<DicomResolvedBatchFrame>,
    cache_result: bool,
    control: Option<&crate::ReadControl>,
    batch_kind: DicomFrameBatchKind,
) -> Result<Vec<DicomFrameBytes>, WsiError> {
    check_read_control(control)?;
    let mut groups: HashMap<usize, (Arc<DicomImage>, Vec<DicomResolvedBatchFrame>)> =
        HashMap::new();
    for meta in metas {
        check_read_control(control)?;
        let image = meta.image.clone();
        let key = Arc::as_ptr(&image) as usize;
        groups
            .entry(key)
            .or_insert_with(|| (image, Vec::new()))
            .1
            .push(meta);
    }

    let mut jobs = Vec::new();
    for (_, (image, mut metas)) in groups {
        check_read_control(control)?;
        let cache_decoded_frame = image.should_cache_decoded_frames_for_batch(metas.len());
        for meta in &mut metas {
            meta.cache_decoded_frame = cache_decoded_frame;
        }
        let frame_indices = metas
            .iter()
            .map(|meta| meta.frame_index)
            .collect::<Vec<_>>();
        let first = &metas[0].req;
        let frames = image.extract_encapsulated_frames_controlled(
            &frame_indices,
            first.level.get(),
            first.col,
            first.row,
            cache_result,
            control,
        )?;
        for meta in metas {
            let frame_index = meta.frame_index;
            let bytes = frames.get(&frame_index).cloned().ok_or_else(|| {
                let req = &meta.req;
                WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: match batch_kind {
                        DicomFrameBatchKind::Cpu => {
                            format!("DICOM batch frame {frame_index} was not extracted")
                        }
                        #[cfg(any(feature = "metal", feature = "cuda"))]
                        DicomFrameBatchKind::Device => {
                            format!("DICOM device batch frame {frame_index} was not extracted")
                        }
                    },
                }
            })?;
            jobs.push((meta, bytes));
        }
    }
    jobs.sort_by_key(|(meta, _)| meta.slot);
    check_read_control(control)?;
    Ok(jobs)
}
