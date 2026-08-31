use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;

use crate::error::WsiError;

use super::check_index_control;
use super::model::{
    DicomEncapsulatedFrames, DicomExtendedOffsetTables, DicomFragmentRef, FastDicomFrameIndex,
};
use super::raw_little_endian::read_exact_at;
use crate::formats::dicom::metadata::invalid_slide;

const MAX_BASIC_OFFSET_TABLE_BYTES: u32 = 64 * 1024 * 1024;
const MAX_EXTENDED_OFFSET_TABLE_BYTES: u32 = 128 * 1024 * 1024;
const TABLE_READ_CHUNK_BYTES: usize = 64 * 1024;

pub(in super::super) fn checked_padded_fragment_len(
    path: &Path,
    frame_index: usize,
    declared_len: u64,
) -> Result<u32, WsiError> {
    if declared_len == 0 || declared_len > u64::from(u32::MAX) {
        return Err(invalid_slide(
            path,
            format!("DICOM frame {frame_index} has invalid extended length {declared_len}"),
        ));
    }
    let padded_len = declared_len
        .checked_add(declared_len & 1)
        .ok_or_else(|| invalid_slide(path, "DICOM extended frame padded length overflow"))?;
    u32::try_from(padded_len).map_err(|_| {
        invalid_slide(
            path,
            format!("DICOM frame {frame_index} padded length {padded_len} exceeds u32"),
        )
    })
}

pub(in super::super) fn build_encapsulated_frame_index(
    path: &Path,
    fragments: Vec<DicomFragmentRef>,
    offset_table: Vec<u32>,
    number_of_frames: u32,
) -> Result<DicomEncapsulatedFrames, WsiError> {
    build_encapsulated_frame_index_with_extended_offsets(
        path,
        fragments,
        offset_table,
        None,
        number_of_frames,
    )
}

fn build_encapsulated_frame_index_with_extended_offsets(
    path: &Path,
    fragments: Vec<DicomFragmentRef>,
    offset_table: Vec<u32>,
    extended_offset_tables: Option<&DicomExtendedOffsetTables>,
    number_of_frames: u32,
) -> Result<DicomEncapsulatedFrames, WsiError> {
    build_encapsulated_frame_index_with_mapping(
        path,
        fragments,
        offset_table,
        extended_offset_tables,
        number_of_frames,
    )
    .map(|index| index.frames)
}

pub(super) fn build_encapsulated_frame_index_with_mapping(
    path: &Path,
    fragments: Vec<DicomFragmentRef>,
    offset_table: Vec<u32>,
    extended_offset_tables: Option<&DicomExtendedOffsetTables>,
    number_of_frames: u32,
) -> Result<FastDicomFrameIndex, WsiError> {
    if number_of_frames == 0 {
        return Err(invalid_slide(path, "DICOM reported zero frames"));
    }
    if fragments.is_empty() {
        return Err(invalid_slide(
            path,
            "DICOM encapsulated pixel data has no fragments",
        ));
    }

    let extended_ranges = extended_offset_tables
        .map(|tables| {
            frame_ranges_from_extended_offsets(path, &fragments, tables, number_of_frames)
        })
        .transpose();
    let (extended_ranges, extended_error) = match extended_ranges {
        Ok(ranges) => (ranges, None),
        Err(error) => (None, Some(error)),
    };

    let fallback_ranges = || -> Result<_, WsiError> {
        if number_of_frames == 1 {
            return Ok((
                std::iter::once(0..fragments.len()).collect(),
                crate::DicomIndexMapping::SingleFrameItems,
            ));
        }
        if !offset_table.is_empty() {
            return frame_ranges_from_basic_offsets(
                path,
                &fragments,
                &offset_table,
                number_of_frames,
            )
            .map(|ranges| (ranges, crate::DicomIndexMapping::BasicOffsetTableItems));
        }
        if fragments.len() == number_of_frames as usize {
            return Ok((
                (0..fragments.len()).map(|index| index..index + 1).collect(),
                crate::DicomIndexMapping::OneFragmentPerFrame,
            ));
        }
        Err(invalid_slide(
            path,
            format!(
                "cannot map {} DICOM fragments to {} frames without a valid offset table",
                fragments.len(),
                number_of_frames
            ),
        ))
    };

    let (frame_ranges, mapping) = if let Some(ranges) = extended_ranges {
        (ranges, crate::DicomIndexMapping::ExtendedOffsetTableItems)
    } else {
        match fallback_ranges() {
            Ok(index) => index,
            Err(fallback_error) => {
                if let Some(extended_error) = extended_error {
                    return Err(invalid_slide(
                        path,
                        format!(
                            "invalid DICOM extended offset table ({extended_error}); {fallback_error}"
                        ),
                    ));
                }
                return Err(fallback_error);
            }
        }
    };

    Ok(FastDicomFrameIndex {
        frames: DicomEncapsulatedFrames {
            fragments,
            frame_ranges,
        },
        mapping,
    })
}

fn frame_ranges_from_basic_offsets(
    path: &Path,
    fragments: &[DicomFragmentRef],
    offset_table: &[u32],
    number_of_frames: u32,
) -> Result<Vec<std::ops::Range<usize>>, WsiError> {
    if offset_table.len() != number_of_frames as usize {
        return Err(invalid_slide(
            path,
            format!(
                "DICOM basic offset table length {} does not match number_of_frames {}",
                offset_table.len(),
                number_of_frames
            ),
        ));
    }
    if offset_table.first().copied() != Some(0) {
        return Err(invalid_slide(
            path,
            "DICOM basic offset table must begin at offset zero",
        ));
    }
    if offset_table.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid_slide(
            path,
            "DICOM basic offset table offsets are not strictly increasing",
        ));
    }

    let base_item_offset = fragments[0].item_offset;
    let fragment_indices_by_offset: HashMap<u64, usize> = fragments
        .iter()
        .enumerate()
        .map(|(index, fragment)| (fragment.item_offset, index))
        .collect();
    let mut start_indices = Vec::with_capacity(offset_table.len());
    for offset in offset_table {
        let target = base_item_offset
            .checked_add(u64::from(*offset))
            .ok_or_else(|| invalid_slide(path, "DICOM basic offset table offset overflow"))?;
        let index = fragment_indices_by_offset
            .get(&target)
            .copied()
            .ok_or_else(|| {
                invalid_slide(
                    path,
                    format!("DICOM basic offset table points to missing fragment offset {offset}"),
                )
            })?;
        start_indices.push(index);
    }
    frame_ranges_from_start_indices(path, start_indices, fragments.len())
}

pub(in super::super) fn frame_ranges_from_extended_offsets(
    path: &Path,
    fragments: &[DicomFragmentRef],
    tables: &DicomExtendedOffsetTables,
    number_of_frames: u32,
) -> Result<Vec<std::ops::Range<usize>>, WsiError> {
    validate_extended_offset_table_shape(path, tables, number_of_frames)?;
    let expected = number_of_frames as usize;

    let base_item_offset = fragments[0].item_offset;
    let fragment_indices_by_offset: HashMap<u64, usize> = fragments
        .iter()
        .enumerate()
        .map(|(index, fragment)| (fragment.item_offset, index))
        .collect();
    let mut start_indices = Vec::with_capacity(expected);
    for offset in &tables.offsets {
        let target = base_item_offset
            .checked_add(*offset)
            .ok_or_else(|| invalid_slide(path, "DICOM extended offset table offset overflow"))?;
        let index = fragment_indices_by_offset
            .get(&target)
            .copied()
            .ok_or_else(|| {
                invalid_slide(
                    path,
                    format!(
                        "DICOM extended offset table points to missing fragment offset {offset}"
                    ),
                )
            })?;
        start_indices.push(index);
    }
    let ranges = frame_ranges_from_start_indices(path, start_indices, fragments.len())?;
    for (frame_index, (range, declared_len)) in ranges.iter().zip(&tables.lengths).enumerate() {
        validate_extended_frame_length(path, fragments, range, frame_index, *declared_len)?;
    }
    Ok(ranges)
}

pub(super) fn validate_extended_offset_table_shape(
    path: &Path,
    tables: &DicomExtendedOffsetTables,
    number_of_frames: u32,
) -> Result<(), WsiError> {
    let expected = number_of_frames as usize;
    if tables.offsets.len() != expected || tables.lengths.len() != expected {
        return Err(invalid_slide(
            path,
            format!(
                "DICOM extended offset table cardinality {}/{} does not match number_of_frames {}",
                tables.offsets.len(),
                tables.lengths.len(),
                number_of_frames
            ),
        ));
    }
    if tables.offsets.first().copied() != Some(0) {
        return Err(invalid_slide(
            path,
            "DICOM extended offset table must begin at offset zero",
        ));
    }
    if tables.offsets.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid_slide(
            path,
            "DICOM extended offset table offsets are not strictly increasing",
        ));
    }
    Ok(())
}

fn frame_ranges_from_start_indices(
    path: &Path,
    start_indices: Vec<usize>,
    fragment_count: usize,
) -> Result<Vec<std::ops::Range<usize>>, WsiError> {
    let mut ranges = Vec::with_capacity(start_indices.len());
    for (frame, start) in start_indices.iter().copied().enumerate() {
        let end = start_indices
            .get(frame + 1)
            .copied()
            .unwrap_or(fragment_count);
        if start >= end || end > fragment_count {
            return Err(invalid_slide(
                path,
                format!("DICOM offset table has invalid fragment range {start}..{end}"),
            ));
        }
        ranges.push(start..end);
    }
    Ok(ranges)
}

fn validate_extended_frame_length(
    path: &Path,
    fragments: &[DicomFragmentRef],
    range: &std::ops::Range<usize>,
    frame_index: usize,
    declared_len: u64,
) -> Result<(), WsiError> {
    let frame_fragments = fragments
        .get(range.clone())
        .ok_or_else(|| invalid_slide(path, "DICOM extended frame fragment range is invalid"))?;
    let first = frame_fragments
        .first()
        .ok_or_else(|| invalid_slide(path, "DICOM extended frame has no fragments"))?;
    let last = frame_fragments
        .last()
        .ok_or_else(|| invalid_slide(path, "DICOM extended frame has no fragments"))?;
    let payload_len = frame_fragments.iter().try_fold(0u64, |total, fragment| {
        total
            .checked_add(u64::from(fragment.len))
            .ok_or_else(|| invalid_slide(path, "DICOM extended frame payload length overflow"))
    })?;
    let minimum_len = payload_len.saturating_sub(frame_fragments.len() as u64);
    let physical_end = last
        .payload_offset
        .checked_add(u64::from(last.len))
        .ok_or_else(|| invalid_slide(path, "DICOM extended frame end offset overflow"))?;
    let maximum_len = physical_end
        .checked_sub(first.item_offset)
        .ok_or_else(|| invalid_slide(path, "DICOM extended frame length underflow"))?;
    if declared_len == 0 || declared_len < minimum_len || declared_len > maximum_len {
        return Err(invalid_slide(
            path,
            format!(
                "DICOM extended offset table length {declared_len} is invalid for frame {frame_index} (expected {minimum_len}..={maximum_len})"
            ),
        ));
    }
    Ok(())
}

pub(in super::super) fn read_extended_offset_tables_le(
    file: &mut File,
    path: &Path,
    offsets_value: Option<u64>,
    lengths_value: Option<u64>,
    table_len: u32,
    control: Option<&crate::ReadControl>,
) -> Result<Option<DicomExtendedOffsetTables>, WsiError> {
    let (Some(offsets_value), Some(lengths_value)) = (offsets_value, lengths_value) else {
        return Ok(None);
    };
    if table_len > MAX_EXTENDED_OFFSET_TABLE_BYTES {
        return Err(invalid_slide(
            path,
            format!(
                "DICOM extended offset table length {table_len} exceeds the supported {MAX_EXTENDED_OFFSET_TABLE_BYTES}-byte limit"
            ),
        ));
    }
    let file_len = file
        .metadata()
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?
        .len();
    read_extended_offset_tables_with_reader(
        file,
        path,
        offsets_value,
        lengths_value,
        table_len,
        file_len,
        control,
    )
}

pub(in super::super) fn read_extended_offset_tables_with_reader<R: Read + Seek>(
    reader: &mut R,
    path: &Path,
    offsets_value: u64,
    lengths_value: u64,
    table_len: u32,
    file_len: u64,
    control: Option<&crate::ReadControl>,
) -> Result<Option<DicomExtendedOffsetTables>, WsiError> {
    check_index_control(control)?;
    for (name, value_offset) in [("offsets", offsets_value), ("lengths", lengths_value)] {
        let value_end = value_offset
            .checked_add(u64::from(table_len))
            .ok_or_else(|| invalid_slide(path, "DICOM extended offset table range overflow"))?;
        if value_end > file_len {
            return Err(invalid_slide(
                path,
                format!(
                    "DICOM extended offset table {name} value range {value_offset}..{value_end} is outside the source file ({file_len} bytes)"
                ),
            ));
        }
    }
    if !table_len.is_multiple_of(8) {
        return Err(invalid_slide(
            path,
            "DICOM extended offset table length is not a multiple of eight",
        ));
    }
    let offsets = read_u64_table_controlled(reader, path, offsets_value, table_len, control)?;
    let lengths = read_u64_table_controlled(reader, path, lengths_value, table_len, control)?;
    check_index_control(control)?;

    Ok(Some(DicomExtendedOffsetTables { offsets, lengths }))
}

fn read_u64_table_controlled<R: Read + Seek>(
    reader: &mut R,
    path: &Path,
    value_offset: u64,
    table_len: u32,
    control: Option<&crate::ReadControl>,
) -> Result<Vec<u64>, WsiError> {
    let entry_count = usize::try_from(table_len / 8)
        .map_err(|_| invalid_slide(path, "DICOM extended offset table length overflow"))?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(entry_count)
        .map_err(|_| invalid_slide(path, "cannot allocate DICOM extended offset table"))?;
    let mut buffer = [0u8; TABLE_READ_CHUNK_BYTES];
    let mut bytes_read = 0u32;
    while bytes_read < table_len {
        check_index_control(control)?;
        let remaining =
            usize::try_from(table_len - bytes_read).expect("u32 DICOM table remainder fits usize");
        let chunk_len = remaining.min(buffer.len());
        let chunk_offset = value_offset
            .checked_add(u64::from(bytes_read))
            .ok_or_else(|| invalid_slide(path, "DICOM extended offset table offset overflow"))?;
        read_exact_at(reader, path, chunk_offset, &mut buffer[..chunk_len])?;
        check_index_control(control)?;
        values.extend(buffer[..chunk_len].chunks_exact(8).map(|chunk| {
            u64::from_le_bytes(
                chunk
                    .try_into()
                    .expect("DICOM extended offset table chunk is 8 bytes"),
            )
        }));
        bytes_read += u32::try_from(chunk_len).expect("bounded table read chunk fits u32");
    }
    Ok(values)
}

pub(in super::super) fn read_basic_offset_table_at(
    file: &mut (impl Read + Seek),
    path: &Path,
    offset: u64,
    len: u32,
    number_of_frames: Option<u32>,
    control: Option<&crate::ReadControl>,
) -> Result<Vec<u32>, WsiError> {
    let entry_count = validate_basic_offset_table_len(path, len, number_of_frames)?;
    check_index_control(control)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(entry_count)
        .map_err(|_| invalid_slide(path, "cannot allocate DICOM basic offset table"))?;
    let mut buffer = [0u8; TABLE_READ_CHUNK_BYTES];
    let mut bytes_read = 0u32;
    while bytes_read < len {
        check_index_control(control)?;
        let remaining = usize::try_from(len - bytes_read)
            .expect("u32 DICOM basic offset table remainder fits usize");
        let chunk_len = remaining.min(buffer.len());
        let chunk_offset = offset
            .checked_add(u64::from(bytes_read))
            .ok_or_else(|| invalid_slide(path, "DICOM basic offset table offset overflow"))?;
        read_exact_at(file, path, chunk_offset, &mut buffer[..chunk_len])?;
        check_index_control(control)?;
        values.extend(buffer[..chunk_len].chunks_exact(4).map(|chunk| {
            u32::from_le_bytes(
                chunk
                    .try_into()
                    .expect("DICOM basic offset table chunk is 4 bytes"),
            )
        }));
        bytes_read += u32::try_from(chunk_len).expect("bounded table read chunk fits u32");
    }
    Ok(values)
}

pub(in super::super) fn validate_basic_offset_table_len(
    path: &Path,
    len: u32,
    number_of_frames: Option<u32>,
) -> Result<usize, WsiError> {
    if !len.is_multiple_of(4) {
        return Err(invalid_slide(
            path,
            format!("DICOM basic offset table has non-u32 length {len}"),
        ));
    }
    if len > MAX_BASIC_OFFSET_TABLE_BYTES {
        return Err(invalid_slide(
            path,
            format!(
                "DICOM basic offset table length {len} exceeds safety limit {MAX_BASIC_OFFSET_TABLE_BYTES}"
            ),
        ));
    }
    if let Some(number_of_frames) = number_of_frames {
        let expected_len = number_of_frames
            .checked_mul(4)
            .ok_or_else(|| invalid_slide(path, "DICOM basic offset table length overflow"))?;
        if len != 0 && len != expected_len {
            return Err(invalid_slide(
                path,
                format!(
                    "DICOM basic offset table length {len} does not match {number_of_frames} frames"
                ),
            ));
        }
    }
    usize::try_from(len / 4)
        .map_err(|_| invalid_slide(path, "DICOM basic offset table length overflow"))
}
