use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::ops::Range;
use std::sync::Arc;

use crate::core::read_control::check_cancelled_opt;
use crate::error::WsiError;

use super::DicomImage;
use crate::formats::dicom::frame_index::{
    self, group_frame_read_spans, preflight_compressed_frame_with_limit,
    scan_encapsulated_frames_controlled, DicomEncapsulatedFrames, DicomFrameReadSpan,
};

impl DicomImage {
    fn validated_encapsulated_frame_range(
        &self,
        encapsulated_frames: &DicomEncapsulatedFrames,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
    ) -> Result<Range<usize>, WsiError> {
        let frame_range = encapsulated_frames
            .frame_ranges
            .get(frame_index as usize)
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!(
                    "encapsulated frame {frame_index} out of range for {} frames",
                    encapsulated_frames.frame_ranges.len()
                ),
            })?
            .clone();
        preflight_compressed_frame_with_limit(
            &self.frame_store.path,
            &encapsulated_frames.fragments[frame_range.clone()],
            self.frame_store.encoded_unit_bytes,
        )?;
        Ok(frame_range)
    }

    pub(in super::super) fn source_encoded_bytes(&self) -> Result<u64, WsiError> {
        if let Some(native) = &self.frame_store.native_pixel_data {
            return Ok(u64::from(native.value_len));
        }
        let frames = self.ensure_encapsulated_frames()?;
        Ok(fragment_payload_bytes(&frames.fragments))
    }

    pub(in super::super) fn extract_encapsulated_frame(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
        cache_result: bool,
    ) -> Result<Arc<Vec<u8>>, WsiError> {
        if !self.transfer_syntax.is_encapsulated() {
            return Err(not_encapsulated_error(level, col, row));
        }
        if cache_result {
            if let Some(bytes) = self
                .frame_store
                .compressed_frame_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&frame_index)
                .cloned()
            {
                return Ok(bytes);
            }
        }
        let encapsulated_frames = self.ensure_encapsulated_frames()?;
        let frame_range = self.validated_encapsulated_frame_range(
            &encapsulated_frames,
            frame_index,
            level,
            col,
            row,
        )?;
        let bytes = Arc::new(frame_index::read_encapsulated_fragments(
            &self.frame_store.path,
            &encapsulated_frames.fragments[frame_range],
        )?);
        if cache_result {
            self.frame_store
                .compressed_frame_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .put(
                    frame_index,
                    bytes.clone(),
                    u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                );
        }
        Ok(bytes)
    }

    pub(in super::super) fn extract_encapsulated_frames_controlled(
        &self,
        frame_indices: &[u32],
        level: u32,
        col: i64,
        row: i64,
        cache_result: bool,
        control: Option<&crate::ReadControl>,
    ) -> Result<HashMap<u32, Arc<Vec<u8>>>, WsiError> {
        check_cancelled_opt(control)?;
        let mut results = HashMap::with_capacity(frame_indices.len());
        if frame_indices.is_empty() {
            return Ok(results);
        }

        let mut missing = Vec::new();
        let mut seen = HashSet::with_capacity(frame_indices.len());
        if cache_result {
            let mut cache = self
                .frame_store
                .compressed_frame_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for &frame_index in frame_indices {
                check_cancelled_opt(control)?;
                if !seen.insert(frame_index) {
                    continue;
                }
                if let Some(bytes) = cache.get(&frame_index).cloned() {
                    results.insert(frame_index, bytes);
                } else {
                    missing.push(frame_index);
                }
            }
        } else {
            for &frame_index in frame_indices {
                check_cancelled_opt(control)?;
                if seen.insert(frame_index) {
                    missing.push(frame_index);
                }
            }
        }

        if missing.is_empty() {
            return Ok(results);
        }

        if !self.transfer_syntax.is_encapsulated() {
            return Err(not_encapsulated_error(level, col, row));
        }

        let encapsulated_frames = self.ensure_encapsulated_frames_controlled(control)?;
        let mut spans = Vec::with_capacity(missing.len());
        for frame_index in missing {
            check_cancelled_opt(control)?;
            let frame_range = self.validated_encapsulated_frame_range(
                &encapsulated_frames,
                frame_index,
                level,
                col,
                row,
            )?;
            spans.push(frame_index::frame_read_span(
                &self.frame_store.path,
                &encapsulated_frames,
                frame_index,
                frame_range,
                level,
                col,
                row,
            )?);
        }

        let mut file = File::open(&self.frame_store.path)
            .map_err(|source| WsiError::io_with_path(&self.frame_store.path, source))?;
        for group in group_frame_read_spans(spans) {
            check_cancelled_opt(control)?;
            for (frame_index, bytes) in frame_index::read_encapsulated_frame_group(
                &self.frame_store.path,
                &mut file,
                &encapsulated_frames,
                &group,
            )? {
                let bytes = Arc::new(bytes);
                if cache_result {
                    self.frame_store
                        .compressed_frame_cache
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .put(
                            frame_index,
                            bytes.clone(),
                            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                        );
                }
                results.insert(frame_index, bytes);
            }
        }

        check_cancelled_opt(control)?;
        Ok(results)
    }

    /// Encoded bytes a single-frame read of `frame_index` holds, when known
    /// without I/O: the native frame size, or the indexed fragment lengths once
    /// the lazy frame index exists.
    pub(in super::super) fn known_encoded_frame_bytes(&self, frame_index: u32) -> Option<u64> {
        if !self.transfer_syntax.is_encapsulated() {
            return Some(self.native_frame_bytes());
        }
        let frames = self.indexed_encapsulated_frames()?;
        let range = frames.frame_ranges.get(frame_index as usize)?;
        Some(fragment_payload_bytes(frames.fragments.get(range.clone())?))
    }

    /// Encoded bytes a batch read of `frame_indices` holds, when known without
    /// I/O. Each frame, capped at `frame_cap`, stays alive until decode, and
    /// coalesced reads also hold the largest read window, Item headers and gaps
    /// between frames included, while frames are copied out of it.
    pub(in super::super) fn known_batch_read_bytes(
        &self,
        frame_indices: &[u32],
        frame_cap: u64,
    ) -> Option<u64> {
        if !self.transfer_syntax.is_encapsulated() {
            // Native frames are read one at a time without a shared window.
            let frames = u64::try_from(frame_indices.len()).unwrap_or(u64::MAX);
            return Some(
                self.native_frame_bytes()
                    .min(frame_cap)
                    .saturating_mul(frames),
            );
        }
        let frames = self.indexed_encapsulated_frames()?;
        let mut retained = 0_u64;
        let mut spans = Vec::with_capacity(frame_indices.len());
        for &frame_index in frame_indices {
            let frame_range = frames.frame_ranges.get(frame_index as usize)?.clone();
            let fragments = frames.fragments.get(frame_range.clone())?;
            retained = retained.saturating_add(fragment_payload_bytes(fragments).min(frame_cap));
            let start = fragments.iter().map(|fragment| fragment.item_offset).min();
            let end = fragments
                .iter()
                .map(|fragment| {
                    fragment
                        .payload_offset
                        .saturating_add(u64::from(fragment.len))
                })
                .max();
            if let (Some(start), Some(end)) = (start, end) {
                spans.push(DicomFrameReadSpan {
                    frame_index,
                    frame_range,
                    start,
                    end,
                });
            }
        }
        let window = group_frame_read_spans(spans)
            .iter()
            .map(|group| group.end.saturating_sub(group.start))
            .max()
            .unwrap_or(0);
        Some(retained.saturating_add(window))
    }

    /// Memory estimates treat an overflowing frame size as unbounded.
    fn native_frame_bytes(&self) -> u64 {
        self.checked_native_frame_bytes().unwrap_or(u64::MAX)
    }

    /// Bytes of one uncompressed frame, or `None` when that overflows `u64`.
    pub(super) fn checked_native_frame_bytes(&self) -> Option<u64> {
        u64::from(self.tile_width)
            .checked_mul(u64::from(self.tile_height))?
            .checked_mul(u64::from(self.samples_per_pixel))?
            .checked_mul(u64::from(self.bit_depth.bits_allocated()).div_ceil(8))
    }

    /// The lazy frame index, once built. Every frame read takes this lock
    /// briefly, so callers wait for it rather than falling back under
    /// concurrency; the only long hold is the first index build, which the
    /// planned read needs anyway.
    fn indexed_encapsulated_frames(&self) -> Option<Arc<DicomEncapsulatedFrames>> {
        self.frame_store
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(in super::super) fn ensure_encapsulated_frames(
        &self,
    ) -> Result<Arc<DicomEncapsulatedFrames>, WsiError> {
        self.ensure_encapsulated_frames_controlled(None)
    }

    pub(in super::super) fn ensure_encapsulated_frames_controlled(
        &self,
        control: Option<&crate::ReadControl>,
    ) -> Result<Arc<DicomEncapsulatedFrames>, WsiError> {
        let Some(control) = control else {
            return self.ensure_encapsulated_frames_with_builder(None, || {
                scan_encapsulated_frames_controlled(
                    &self.frame_store.path,
                    &self.transfer_syntax_uid,
                    self.number_of_frames,
                    None,
                )
            });
        };

        let (deferred_control, deferred_diagnostics) = control.defer_diagnostics();
        let result = self.ensure_encapsulated_frames_with_builder(Some(&deferred_control), || {
            scan_encapsulated_frames_controlled(
                &self.frame_store.path,
                &self.transfer_syntax_uid,
                self.number_of_frames,
                Some(&deferred_control),
            )
        });
        if result.is_ok() {
            deferred_diagnostics.flush();
        }
        result
    }

    pub(in super::super) fn ensure_encapsulated_frames_with_builder(
        &self,
        control: Option<&crate::ReadControl>,
        build: impl FnOnce() -> Result<DicomEncapsulatedFrames, WsiError>,
    ) -> Result<Arc<DicomEncapsulatedFrames>, WsiError> {
        let reuse_started = control
            .filter(|control| control.diagnostics_enabled())
            .map(|_| std::time::Instant::now());
        let mut guard = self
            .frame_store
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        check_cancelled_opt(control)?;
        if let Some(frames) = &*guard {
            let frames = frames.clone();
            tracing::trace!(
                path = %self.frame_store.path.display(),
                outcome = "reused",
                "reused DICOM encapsulated frame index"
            );
            drop(guard);
            if let (Some(control), Some(started)) = (control, reuse_started) {
                control.record_diagnostic(crate::DicomIndexDiagnostic::new(
                    crate::DicomIndexOutcome::Reused,
                    started.elapsed(),
                ));
            }
            return Ok(frames);
        }
        let started = tracing::enabled!(tracing::Level::DEBUG).then(std::time::Instant::now);
        let result = build();
        check_cancelled_opt(control)?;
        let frames = Arc::new(result?);
        *guard = Some(frames.clone());
        if let Err(error) = check_cancelled_opt(control) {
            *guard = None;
            return Err(error);
        }
        if let Some(started) = started {
            tracing::debug!(
                path = %self.frame_store.path.display(),
                outcome = "built",
                elapsed_us = started.elapsed().as_micros(),
                "published DICOM encapsulated frame index"
            );
        }
        Ok(frames)
    }
}

/// Native Pixel Data has no encapsulated frames to extract.
fn not_encapsulated_error(level: u32, col: i64, row: i64) -> WsiError {
    WsiError::TileRead {
        col,
        row,
        level,
        reason: "pixel data is not encapsulated".into(),
    }
}

fn fragment_payload_bytes(
    fragments: &[crate::formats::dicom::frame_index::model::DicomFragmentRef],
) -> u64 {
    fragments
        .iter()
        .map(|fragment| u64::from(fragment.len))
        .sum()
}
