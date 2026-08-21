use std::collections::HashMap;
use std::fs::File;
#[cfg(test)]
use std::io::{Read, Seek};
use std::sync::Arc;

use dicom_dictionary_std::tags;

use crate::error::WsiError;

use super::DicomImage;
use crate::formats::dicom::backend::is_encapsulated_transfer_syntax;
use crate::formats::dicom::frame_index::{
    self, group_frame_read_spans, preflight_compressed_lengths, reopen_dicom_object,
    scan_encapsulated_frames_controlled, DicomEncapsulatedFrames,
};
#[cfg(test)]
use crate::formats::dicom::frame_index::{DicomFragmentRef, DicomFrameReadGroup};
use crate::formats::dicom::metadata::optional_u32;

impl DicomImage {
    pub(in super::super) fn extract_encapsulated_frame(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
        cache_result: bool,
    ) -> Result<Arc<Vec<u8>>, WsiError> {
        if is_encapsulated_transfer_syntax(&self.transfer_syntax_uid) {
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
                })?;
            let bytes = Arc::new(frame_index::read_encapsulated_fragments(
                &self.frame_store.path,
                &encapsulated_frames.fragments[frame_range.start..frame_range.end],
            )?);
            if cache_result {
                self.frame_store
                    .compressed_frame_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .put(frame_index, bytes.clone());
            }
            return Ok(bytes);
        }

        let obj = reopen_dicom_object(&self.frame_store.path)?;
        let pixel_data = obj
            .element(tags::PIXEL_DATA)
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!("missing pixel data: {err}"),
            })?;
        let fragments = pixel_data.fragments().ok_or_else(|| WsiError::TileRead {
            col,
            row,
            level,
            reason: "pixel data is not encapsulated".into(),
        })?;
        let number_of_frames = optional_u32(&obj, tags::NUMBER_OF_FRAMES)
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: err.to_string(),
            })?
            .unwrap_or(1);

        if number_of_frames == 1 && fragments.len() > 1 {
            let total_len = preflight_compressed_lengths(
                &self.frame_store.path,
                fragments.iter().map(Vec::len),
            )?;
            let mut data = Vec::new();
            data.try_reserve_exact(total_len)
                .map_err(|_| WsiError::ResourceLimit {
                    resource: "compressed DICOM frame",
                    requested: total_len as u64,
                    limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
                })?;
            for fragment in fragments {
                data.extend_from_slice(fragment);
            }
            return Ok(Arc::new(data));
        }

        let fragment = fragments
            .get(frame_index as usize)
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!(
                    "encapsulated frame {frame_index} out of range for {} fragments",
                    fragments.len()
                ),
            })?;
        let total_len = preflight_compressed_lengths(&self.frame_store.path, [fragment.len()])?;
        let mut data = Vec::new();
        data.try_reserve_exact(total_len)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "compressed DICOM frame",
                requested: total_len as u64,
                limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
            })?;
        data.extend_from_slice(fragment);
        Ok(Arc::new(data))
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
        if let Some(control) = control {
            control.check_cancelled()?;
        }
        let mut results = HashMap::with_capacity(frame_indices.len());
        if frame_indices.is_empty() {
            return Ok(results);
        }

        let mut missing = Vec::new();
        if cache_result {
            let mut cache = self
                .frame_store
                .compressed_frame_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for &frame_index in frame_indices {
                if let Some(control) = control {
                    control.check_cancelled()?;
                }
                if results.contains_key(&frame_index) {
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
                if let Some(control) = control {
                    control.check_cancelled()?;
                }
                if !results.contains_key(&frame_index) {
                    missing.push(frame_index);
                }
            }
        }

        if missing.is_empty() {
            return Ok(results);
        }

        if !is_encapsulated_transfer_syntax(&self.transfer_syntax_uid) {
            for frame_index in missing {
                let bytes =
                    self.extract_encapsulated_frame(frame_index, level, col, row, cache_result)?;
                results.insert(frame_index, bytes);
            }
            return Ok(results);
        }

        let encapsulated_frames = self.ensure_encapsulated_frames_controlled(control)?;
        let mut spans = Vec::with_capacity(missing.len());
        for frame_index in missing {
            if let Some(control) = control {
                control.check_cancelled()?;
            }
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

        let mut file =
            File::open(&self.frame_store.path).map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: self.frame_store.path.clone(),
            })?;
        for group in group_frame_read_spans(spans) {
            if let Some(control) = control {
                control.check_cancelled()?;
            }
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
                        .put(frame_index, bytes.clone());
                }
                results.insert(frame_index, bytes);
            }
        }

        if let Some(control) = control {
            control.check_cancelled()?;
        }
        Ok(results)
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
        if let Some(control) = control {
            control.check_cancelled()?;
        }
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
        if let Some(control) = control {
            control.check_cancelled()?;
        }
        let frames = Arc::new(result?);
        *guard = Some(frames.clone());
        if let Some(control) = control {
            if let Err(error) = control.check_cancelled() {
                *guard = None;
                return Err(error);
            }
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

    #[cfg(test)]
    pub(in super::super) fn read_encapsulated_fragments(
        &self,
        fragments: &[DicomFragmentRef],
    ) -> Result<Vec<u8>, WsiError> {
        frame_index::read_encapsulated_fragments(&self.frame_store.path, fragments)
    }

    #[cfg(test)]
    pub(in super::super) fn read_encapsulated_frame_group<R: Read + Seek>(
        &self,
        file: &mut R,
        encapsulated_frames: &DicomEncapsulatedFrames,
        group: &DicomFrameReadGroup,
    ) -> Result<Vec<(u32, Vec<u8>)>, WsiError> {
        frame_index::read_encapsulated_frame_group(
            &self.frame_store.path,
            file,
            encapsulated_frames,
            group,
        )
    }
}
