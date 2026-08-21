use std::fs::File;
use std::io::{Read, Seek};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use crate::error::WsiError;

use super::model::{DicomEncapsulatedFrames, DicomFragmentRef};
use super::raw_little_endian::read_exact_at;
use super::validation::preflight_compressed_frame;
use super::DICOM_ITEM_TAG_LE;
use crate::formats::dicom::image::{
    BATCH_FRAME_READ_MAX_GAP_BYTES, BATCH_FRAME_READ_MAX_SPAN_BYTES,
};
use crate::formats::dicom::metadata::invalid_slide;

#[derive(Debug)]
pub(in super::super) struct DicomFrameReadSpan {
    pub(in super::super) frame_index: u32,
    pub(in super::super) frame_range: Range<usize>,
    pub(in super::super) start: u64,
    pub(in super::super) end: u64,
}

#[derive(Debug)]
pub(in super::super) struct DicomFrameReadGroup {
    pub(in super::super) start: u64,
    pub(in super::super) end: u64,
    pub(in super::super) spans: Vec<DicomFrameReadSpan>,
}

pub(in super::super) fn group_frame_read_spans(
    mut spans: Vec<DicomFrameReadSpan>,
) -> Vec<DicomFrameReadGroup> {
    spans.sort_by_key(|span| span.start);
    let mut groups: Vec<DicomFrameReadGroup> = Vec::new();
    for span in spans {
        let Some(current) = groups.last_mut() else {
            groups.push(DicomFrameReadGroup {
                start: span.start,
                end: span.end,
                spans: vec![span],
            });
            continue;
        };
        let gap = span.start.saturating_sub(current.end);
        let merged_end = current.end.max(span.end);
        let merged_len = merged_end.saturating_sub(current.start);
        if gap <= BATCH_FRAME_READ_MAX_GAP_BYTES && merged_len <= BATCH_FRAME_READ_MAX_SPAN_BYTES {
            current.end = merged_end;
            current.spans.push(span);
        } else {
            groups.push(DicomFrameReadGroup {
                start: span.start,
                end: span.end,
                spans: vec![span],
            });
        }
    }
    groups
}

pub(in super::super) fn copy_fragments_from_window(
    path: &Path,
    window_start: u64,
    window: &[u8],
    fragments: &[DicomFragmentRef],
) -> Result<Vec<u8>, WsiError> {
    let total_len = preflight_compressed_frame(path, fragments)?;
    let mut data = Vec::new();
    data.try_reserve_exact(total_len)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "compressed DICOM frame",
            requested: total_len as u64,
            limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
        })?;
    for fragment in fragments {
        let rel_start = fragment
            .payload_offset
            .checked_sub(window_start)
            .ok_or_else(|| invalid_slide(path, "DICOM batch fragment offset underflow"))?;
        let rel_start = usize::try_from(rel_start)
            .map_err(|_| invalid_slide(path, "DICOM batch fragment offset overflow"))?;
        let rel_end = rel_start
            .checked_add(fragment.len as usize)
            .ok_or_else(|| invalid_slide(path, "DICOM batch fragment length overflow"))?;
        let payload = window
            .get(rel_start..rel_end)
            .ok_or_else(|| invalid_slide(path, "DICOM batch fragment outside read window"))?;
        data.extend_from_slice(payload);
    }
    Ok(data)
}

pub(in super::super) fn read_encapsulated_fragments(
    path: &Path,
    fragments: &[DicomFragmentRef],
) -> Result<Vec<u8>, WsiError> {
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    read_encapsulated_fragments_with_file(&mut file, path, fragments)
}

fn read_encapsulated_fragments_with_file(
    file: &mut File,
    path: &Path,
    fragments: &[DicomFragmentRef],
) -> Result<Vec<u8>, WsiError> {
    let total_len = preflight_compressed_frame(path, fragments)?;
    validate_encapsulated_fragment_headers_with_file(file, path, fragments)?;
    let mut data = Vec::new();
    data.try_reserve_exact(total_len)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "compressed DICOM frame",
            requested: total_len as u64,
            limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
        })?;
    for fragment in fragments {
        let start = data.len();
        let end = start
            .checked_add(fragment.len as usize)
            .ok_or_else(|| invalid_slide(path, "DICOM compressed frame length overflow"))?;
        data.resize(end, 0);
        read_exact_at(file, path, fragment.payload_offset, &mut data[start..])?;
    }
    Ok(data)
}

pub(in super::super) fn frame_read_span(
    path: &Path,
    encapsulated_frames: &DicomEncapsulatedFrames,
    frame_index: u32,
    frame_range: Range<usize>,
    level: u32,
    col: i64,
    row: i64,
) -> Result<DicomFrameReadSpan, WsiError> {
    let fragments = encapsulated_frames
        .fragments
        .get(frame_range.clone())
        .ok_or_else(|| WsiError::TileRead {
            col,
            row,
            level,
            reason: format!("encapsulated frame {frame_index} has invalid fragment range"),
        })?;
    preflight_compressed_frame(path, fragments)?;
    let first = fragments.first().ok_or_else(|| WsiError::TileRead {
        col,
        row,
        level,
        reason: format!("encapsulated frame {frame_index} has no fragments"),
    })?;
    let mut start = first.item_offset;
    let mut end = first
        .payload_offset
        .checked_add(first.len as u64)
        .ok_or_else(|| WsiError::TileRead {
            col,
            row,
            level,
            reason: format!("encapsulated frame {frame_index} byte span overflow"),
        })?;
    for fragment in &fragments[1..] {
        start = start.min(fragment.item_offset);
        let fragment_end = fragment
            .payload_offset
            .checked_add(fragment.len as u64)
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!("encapsulated frame {frame_index} byte span overflow"),
            })?;
        end = end.max(fragment_end);
    }
    Ok(DicomFrameReadSpan {
        frame_index,
        frame_range,
        start,
        end,
    })
}

pub(in super::super) fn read_encapsulated_frame_group<R: Read + Seek>(
    path: &Path,
    file: &mut R,
    encapsulated_frames: &DicomEncapsulatedFrames,
    group: &DicomFrameReadGroup,
) -> Result<Vec<(u32, Vec<u8>)>, WsiError> {
    let span_len = group
        .end
        .checked_sub(group.start)
        .ok_or_else(|| invalid_slide(path, "DICOM batch frame read span underflow"))?;
    let span_len = usize::try_from(span_len)
        .map_err(|_| invalid_slide(path, "DICOM batch frame read span overflow"))?;
    if span_len as u64 > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
        return Err(WsiError::ResourceLimit {
            resource: "compressed DICOM batch read span",
            requested: span_len as u64,
            limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
        });
    }
    let mut window = Vec::new();
    window
        .try_reserve_exact(span_len)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "compressed DICOM batch read span",
            requested: span_len as u64,
            limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
        })?;
    window.resize(span_len, 0);
    read_exact_at(file, path, group.start, &mut window)?;

    group
        .spans
        .iter()
        .map(|span| {
            let fragments = encapsulated_frames
                .fragments
                .get(span.frame_range.clone())
                .ok_or_else(|| {
                    invalid_slide(path, "DICOM batch frame fragment range out of bounds")
                })?;
            for fragment in fragments {
                let relative_start =
                    fragment
                        .item_offset
                        .checked_sub(group.start)
                        .ok_or_else(|| {
                            invalid_slide(path, "DICOM batch fragment Item offset underflow")
                        })?;
                let relative_start = usize::try_from(relative_start).map_err(|_| {
                    invalid_slide(path, "DICOM batch fragment Item offset overflow")
                })?;
                let relative_end = relative_start.checked_add(8).ok_or_else(|| {
                    invalid_slide(path, "DICOM batch fragment Item header overflow")
                })?;
                let header = window.get(relative_start..relative_end).ok_or_else(|| {
                    invalid_slide(
                        path,
                        "DICOM batch fragment Item header is outside the read window",
                    )
                })?;
                validate_encapsulated_fragment_header(path, fragment, header)?;
            }
            let data = copy_fragments_from_window(path, group.start, &window, fragments)?;
            Ok((span.frame_index, data))
        })
        .collect()
}

fn validate_encapsulated_fragment_headers_with_file(
    file: &mut (impl Read + Seek),
    path: &Path,
    fragments: &[DicomFragmentRef],
) -> Result<(), WsiError> {
    for fragment in fragments {
        let mut header = [0u8; 8];
        read_exact_at(file, path, fragment.item_offset, &mut header)?;
        validate_encapsulated_fragment_header(path, fragment, &header)?;
    }
    Ok(())
}

fn validate_encapsulated_fragment_header(
    path: &Path,
    fragment: &DicomFragmentRef,
    header: &[u8],
) -> Result<(), WsiError> {
    let tag = header
        .get(..4)
        .ok_or_else(|| invalid_slide(path, "truncated DICOM fragment Item tag"))?;
    let length = header
        .get(4..8)
        .ok_or_else(|| invalid_slide(path, "truncated DICOM fragment Item length"))?;
    let length = u32::from_le_bytes(
        length
            .try_into()
            .expect("validated DICOM fragment Item length is four bytes"),
    );
    if tag != DICOM_ITEM_TAG_LE || length != fragment.len {
        return Err(invalid_slide(
            path,
            format!(
                "DICOM fragment Item header at byte {} does not match its indexed length",
                fragment.item_offset
            ),
        ));
    }
    Ok(())
}
