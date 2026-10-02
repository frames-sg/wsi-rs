use super::*;
use crate::core::registry::ManagedSlideReader;
use crate::core::types::{RegionRequest, TileViewRequest};

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
        let mut frames = std::collections::HashSet::with_capacity(reqs.len());
        let mut known = 0_u64;
        let mut unknown = false;
        for req in reqs {
            if !frames.insert((req.level.get(), req.col, req.row)) {
                continue;
            }
            match self.known_frame_encoded_bytes(req) {
                Some(bytes) => known = known.saturating_add(bytes),
                None => unknown = true,
            }
        }
        // Frames not indexed yet share the one-unit bound batches always had.
        Ok(if unknown {
            known.max(self.slide.encoded_unit_bytes)
        } else {
            known
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
        let level = self.slide.levels.get(req.level.get() as usize)?;
        let col = u32::try_from(req.col).ok()?;
        let row = u32::try_from(req.row).ok()?;
        if col >= level.tiles_across || row >= level.tiles_down {
            return None;
        }
        let Some((image, frame_index)) = level
            .image_for_tile(col, row)
            .and_then(|image| image.frame_index(col, row).map(|index| (image, index)))
        else {
            return Some(0);
        };
        image
            .known_encoded_frame_bytes(frame_index)
            .map(|bytes| bytes.min(self.slide.encoded_unit_bytes))
    }
}
