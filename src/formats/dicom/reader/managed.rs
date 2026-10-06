use super::*;
use crate::core::registry::ManagedSlideReader;
use crate::core::types::{RegionRequest, TileViewRequest};
use crate::formats::dicom::DicomImage;

impl ManagedSlideReader for DicomReader {
    fn read_tiles_cpu_fastpath(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
    ) -> Option<Result<Vec<CpuTile>, WsiError>> {
        use super::batch_plan::{
            DicomBatchPlanMode, DicomBatchPlanner, DicomResolvedBatchPlanEntry,
        };
        let planner = DicomBatchPlanner::new(&self.slide, control, DicomBatchPlanMode::Cpu);
        let mut tiles = Vec::new();
        for (slot, req) in reqs.iter().enumerate() {
            match planner.resolve(slot, req, |_| true) {
                Ok(
                    DicomResolvedBatchPlanEntry::CachedFrame(_, tile)
                    | DicomResolvedBatchPlanEntry::Black(_, tile),
                ) => {
                    // A cold miss declines without allocating an unused batch.
                    if tiles.is_empty() {
                        tiles.reserve_exact(reqs.len());
                    }
                    tiles.push(tile);
                }
                Ok(_) => return None,
                Err(error) => return Some(Err(error)),
            }
        }
        Some(Ok(tiles))
    }

    #[cfg(feature = "metal")]
    fn read_metal_with_context(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::metal::MetalBackendSessions,
        context: &crate::core::limits::ReadExecutionContext<'_>,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        self.read_metal_admitted(reqs, session, context)
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn prepare_adaptive_jp2k(
        &self,
        reqs: &[TileRequest],
        workers: usize,
        control: Option<&crate::ReadControl>,
    ) -> Option<Result<crate::decode::jp2k::PreparedJp2kBatch, WsiError>> {
        Some((|| {
            // Strict planning deliberately bypasses decoded-frame cache entries.
            let frames = self.strict_jp2k_frames(reqs, control)?;
            let jobs = frames
                .iter()
                .map(|(frame, bytes)| crate::decode::jp2k::Jp2kDecodeJob {
                    data: std::borrow::Cow::Borrowed(bytes.as_slice()),
                    expected_width: frame.actual_width,
                    expected_height: frame.actual_height,
                    rgb_color_space: !crate::formats::dicom::decode::jp2k_photometric_is_ycbcr(
                        &frame.image.photometric_interpretation,
                    ),
                    backend: BackendRequest::Cpu,
                })
                .collect::<Vec<_>>();
            crate::decode::jp2k::PreparedJp2kBatch::new(&jobs, workers)
        })())
    }

    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError> {
        Ok(self
            .known_frame_encoded_bytes(req)
            .unwrap_or(self.slide.encoded_unit_bytes))
    }
    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        let mut seen = std::collections::HashSet::with_capacity(reqs.len());
        let mut images: Vec<(Arc<DicomImage>, Vec<u32>)> = Vec::new();
        let mut unknown = false;
        for req in reqs {
            let key = (
                req.scene.get(),
                req.series.get(),
                req.level.get(),
                req.plane.get(),
                req.col,
                req.row,
            );
            if !seen.insert(key) {
                continue;
            }
            match self.frame_for_request(req) {
                FrameLookup::Frame(image, frame_index) => {
                    match images
                        .iter_mut()
                        .find(|(known, _)| Arc::ptr_eq(known, &image))
                    {
                        Some((_, frames)) => frames.push(frame_index),
                        None => images.push((image, vec![frame_index])),
                    }
                }
                FrameLookup::Gap => {}
                FrameLookup::Unknown => unknown = true,
            }
        }
        let mut bytes = 0_u64;
        for (image, frame_indices) in &images {
            match image.known_batch_read_bytes(frame_indices, self.slide.encoded_unit_bytes) {
                Some(image_bytes) => bytes = bytes.saturating_add(image_bytes),
                None => unknown = true,
            }
        }
        // Frames not indexed yet reserve at least one encoded unit.
        Ok(if unknown {
            bytes.max(self.slide.encoded_unit_bytes)
        } else {
            bytes
        })
    }
    fn display_tile_encoded_upper_bound(&self, _: &TileViewRequest) -> Result<u64, WsiError> {
        Ok(self.slide.encoded_unit_bytes)
    }
    fn associated_encoded_upper_bound(&self, _: &str) -> Result<u64, WsiError> {
        Ok(self.slide.encoded_unit_bytes)
    }
    fn region_fastpath_encoded_upper_bound(&self, _: &RegionRequest) -> Result<u64, WsiError> {
        Ok(self.slide.encoded_unit_bytes)
    }
}

impl DicomReader {
    /// Encoded bytes a read of `req`'s frame holds, when known. Admission
    /// reserves this per read, so a frame's indexed length lets concurrent
    /// single-tile reads proceed together instead of each reserving the whole
    /// per-unit limit. Sparse gaps decode no encoded bytes. `None` covers
    /// frames not indexed yet and requests the read will reject.
    fn known_frame_encoded_bytes(&self, req: &TileRequest) -> Option<u64> {
        match self.frame_for_request(req) {
            FrameLookup::Frame(image, frame_index) => image
                .known_encoded_frame_bytes(frame_index)
                .map(|bytes| bytes.min(self.slide.encoded_unit_bytes)),
            FrameLookup::Gap => Some(0),
            FrameLookup::Unknown => None,
        }
    }

    fn frame_for_request(&self, req: &TileRequest) -> FrameLookup {
        let Some(level) = self.slide.levels.get(req.level.get() as usize) else {
            return FrameLookup::Unknown;
        };
        let (Ok(col), Ok(row)) = (u32::try_from(req.col), u32::try_from(req.row)) else {
            return FrameLookup::Unknown;
        };
        if col >= level.tiles_across || row >= level.tiles_down {
            return FrameLookup::Unknown;
        }
        level
            .image_for_tile(col, row)
            .and_then(|image| image.frame_index(col, row).map(|index| (image, index)))
            .map_or(FrameLookup::Gap, |(image, index)| {
                FrameLookup::Frame(image, index)
            })
    }
}

/// Where a tile request's encoded bytes live.
enum FrameLookup {
    Frame(Arc<DicomImage>, u32),
    /// A sparse-grid position with no stored frame decodes no encoded bytes.
    Gap,
    /// Requests the read will reject.
    Unknown,
}
