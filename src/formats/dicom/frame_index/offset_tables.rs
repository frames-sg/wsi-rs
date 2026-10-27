use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use crate::core::read_control::check_cancelled_opt;
use crate::error::WsiError;

use super::model::{
    DicomEncapsulatedFrames, DicomExtendedOffsetTables, DicomFragmentRef, FastDicomFrameIndex,
};
use super::raw_little_endian::read_exact_at;

const MAX_BASIC_OFFSET_TABLE_BYTES: u32 = 64 * 1024 * 1024;
const MAX_EXTENDED_OFFSET_TABLE_BYTES: u32 = 128 * 1024 * 1024;
const TABLE_READ_CHUNK_BYTES: usize = 64 * 1024;
const BASIC_TABLE: &str = "basic";
const EXTENDED_TABLE: &str = "extended";

pub(in super::super) fn checked_padded_fragment_len(
    path: &Path,
    frame_index: usize,
    declared_len: u64,
) -> Result<u32, WsiError> {
    if declared_len == 0 || declared_len > u64::from(u32::MAX) {
        return Err(WsiError::invalid_slide(
            path,
            format!("DICOM frame {frame_index} has invalid extended length {declared_len}"),
        ));
    }
    let padded_len = declared_len.checked_add(declared_len & 1).ok_or_else(|| {
        WsiError::invalid_slide(path, "DICOM extended frame padded length overflow")
    })?;
    u32::try_from(padded_len).map_err(|_| {
        WsiError::invalid_slide(
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
        return Err(WsiError::invalid_slide(path, "DICOM reported zero frames"));
    }
    if fragments.is_empty() {
        return Err(WsiError::invalid_slide(
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
        Err(WsiError::invalid_slide(
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
                    return Err(WsiError::invalid_slide(
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
        return Err(WsiError::invalid_slide(
            path,
            format!(
                "DICOM basic offset table length {} does not match number_of_frames {}",
                offset_table.len(),
                number_of_frames
            ),
        ));
    }
    validate_offsets(path, BASIC_TABLE, offset_table)?;
    let start_indices = start_indices_from_offsets(path, BASIC_TABLE, fragments, offset_table)?;
    frame_ranges_from_start_indices(path, start_indices, fragments.len())
}

pub(in super::super) fn frame_ranges_from_extended_offsets(
    path: &Path,
    fragments: &[DicomFragmentRef],
    tables: &DicomExtendedOffsetTables,
    number_of_frames: u32,
) -> Result<Vec<std::ops::Range<usize>>, WsiError> {
    validate_extended_offset_table_shape(path, tables, number_of_frames)?;
    let start_indices =
        start_indices_from_offsets(path, EXTENDED_TABLE, fragments, &tables.offsets)?;
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
        return Err(WsiError::invalid_slide(
            path,
            format!(
                "DICOM extended offset table cardinality {}/{} does not match number_of_frames {}",
                tables.offsets.len(),
                tables.lengths.len(),
                number_of_frames
            ),
        ));
    }
    validate_offsets(path, EXTENDED_TABLE, &tables.offsets)
}

/// Checks that a `table` ("basic" or "extended") offset table starts at zero
/// and strictly increases.
fn validate_offsets<T: Copy + PartialOrd + Into<u64>>(
    path: &Path,
    table: &str,
    offsets: &[T],
) -> Result<(), WsiError> {
    if offsets.first().copied().map(Into::<u64>::into) != Some(0) {
        return Err(WsiError::invalid_slide(
            path,
            format!("DICOM {table} offset table must begin at offset zero"),
        ));
    }
    if offsets.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(WsiError::invalid_slide(
            path,
            format!("DICOM {table} offset table offsets are not strictly increasing"),
        ));
    }
    Ok(())
}

/// The fragment each offset points at. Offsets count from the first
/// fragment's Item header, so `fragments` must not be empty.
///
/// Callers validate `offsets` as strictly increasing and fragments are listed
/// in file order, so one forward cursor finds every target.
fn start_indices_from_offsets<T: Copy + Into<u64>>(
    path: &Path,
    table: &str,
    fragments: &[DicomFragmentRef],
    offsets: &[T],
) -> Result<Vec<usize>, WsiError> {
    let base_item_offset = fragments[0].item_offset;
    let mut start_indices = Vec::with_capacity(offsets.len());
    let mut index = 0;
    for &offset in offsets {
        let offset: u64 = offset.into();
        let target = base_item_offset.checked_add(offset).ok_or_else(|| {
            WsiError::invalid_slide(path, format!("DICOM {table} offset table offset overflow"))
        })?;
        while fragments
            .get(index)
            .is_some_and(|fragment| fragment.item_offset < target)
        {
            index += 1;
        }
        if fragments
            .get(index)
            .is_none_or(|fragment| fragment.item_offset != target)
        {
            return Err(WsiError::invalid_slide(
                path,
                format!("DICOM {table} offset table points to missing fragment offset {offset}"),
            ));
        }
        start_indices.push(index);
    }
    Ok(start_indices)
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
            return Err(WsiError::invalid_slide(
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
    let frame_fragments = fragments.get(range.clone()).ok_or_else(|| {
        WsiError::invalid_slide(path, "DICOM extended frame fragment range is invalid")
    })?;
    let first = frame_fragments
        .first()
        .ok_or_else(|| WsiError::invalid_slide(path, "DICOM extended frame has no fragments"))?;
    let last = frame_fragments
        .last()
        .ok_or_else(|| WsiError::invalid_slide(path, "DICOM extended frame has no fragments"))?;
    let payload_len = frame_fragments.iter().try_fold(0u64, |total, fragment| {
        total.checked_add(u64::from(fragment.len)).ok_or_else(|| {
            WsiError::invalid_slide(path, "DICOM extended frame payload length overflow")
        })
    })?;
    let minimum_len = payload_len.saturating_sub(frame_fragments.len() as u64);
    let physical_end = last
        .payload_offset
        .checked_add(u64::from(last.len))
        .ok_or_else(|| WsiError::invalid_slide(path, "DICOM extended frame end offset overflow"))?;
    let maximum_len = physical_end
        .checked_sub(first.item_offset)
        .ok_or_else(|| WsiError::invalid_slide(path, "DICOM extended frame length underflow"))?;
    if declared_len == 0 || declared_len < minimum_len || declared_len > maximum_len {
        return Err(WsiError::invalid_slide(
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
    file_len: u64,
    control: Option<&crate::ReadControl>,
) -> Result<Option<DicomExtendedOffsetTables>, WsiError> {
    let (Some(offsets_value), Some(lengths_value)) = (offsets_value, lengths_value) else {
        return Ok(None);
    };
    if table_len > MAX_EXTENDED_OFFSET_TABLE_BYTES {
        return Err(WsiError::invalid_slide(
            path,
            format!(
                "DICOM extended offset table length {table_len} exceeds the supported {MAX_EXTENDED_OFFSET_TABLE_BYTES}-byte limit"
            ),
        ));
    }
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
    check_cancelled_opt(control)?;
    for (name, value_offset) in [("offsets", offsets_value), ("lengths", lengths_value)] {
        let value_end = value_offset
            .checked_add(u64::from(table_len))
            .ok_or_else(|| {
                WsiError::invalid_slide(path, "DICOM extended offset table range overflow")
            })?;
        if value_end > file_len {
            return Err(WsiError::invalid_slide(
                path,
                format!(
                    "DICOM extended offset table {name} value range {value_offset}..{value_end} is outside the source file ({file_len} bytes)"
                ),
            ));
        }
    }
    if !table_len.is_multiple_of(8) {
        return Err(WsiError::invalid_slide(
            path,
            "DICOM extended offset table length is not a multiple of eight",
        ));
    }
    let offsets = read_le_table(
        reader,
        path,
        EXTENDED_TABLE,
        offsets_value,
        table_len,
        u64::from_le_bytes,
        control,
    )?;
    let lengths = read_le_table(
        reader,
        path,
        EXTENDED_TABLE,
        lengths_value,
        table_len,
        u64::from_le_bytes,
        control,
    )?;
    check_cancelled_opt(control)?;

    Ok(Some(DicomExtendedOffsetTables { offsets, lengths }))
}

pub(in super::super) fn read_basic_offset_table_at(
    file: &mut (impl Read + Seek),
    path: &Path,
    offset: u64,
    len: u32,
    number_of_frames: Option<u32>,
    control: Option<&crate::ReadControl>,
) -> Result<Vec<u32>, WsiError> {
    validate_basic_offset_table_len(path, len, number_of_frames)?;
    read_le_table(
        file,
        path,
        BASIC_TABLE,
        offset,
        len,
        u32::from_le_bytes,
        control,
    )
}

/// The `table_len / N` little-endian entries of a `table` offset table at
/// `value_offset`, read in bounded chunks with cancellation checked around
/// each read. Callers check that `table_len` is a multiple of `N`.
fn read_le_table<T, const N: usize>(
    reader: &mut (impl Read + Seek),
    path: &Path,
    table: &str,
    value_offset: u64,
    table_len: u32,
    from_le_bytes: impl Fn([u8; N]) -> T,
    control: Option<&crate::ReadControl>,
) -> Result<Vec<T>, WsiError> {
    check_cancelled_opt(control)?;
    let entry_count = usize::try_from(table_len).map(|len| len / N).map_err(|_| {
        WsiError::invalid_slide(path, format!("DICOM {table} offset table length overflow"))
    })?;
    let mut values = Vec::new();
    values.try_reserve_exact(entry_count).map_err(|_| {
        WsiError::invalid_slide(path, format!("cannot allocate DICOM {table} offset table"))
    })?;
    let mut buffer = [0u8; TABLE_READ_CHUNK_BYTES];
    let mut bytes_read = 0u32;
    while bytes_read < table_len {
        check_cancelled_opt(control)?;
        let remaining =
            usize::try_from(table_len - bytes_read).expect("u32 DICOM table remainder fits usize");
        let chunk_len = remaining.min(buffer.len());
        let chunk_offset = value_offset
            .checked_add(u64::from(bytes_read))
            .ok_or_else(|| {
                WsiError::invalid_slide(path, format!("DICOM {table} offset table offset overflow"))
            })?;
        read_exact_at(reader, path, chunk_offset, &mut buffer[..chunk_len])?;
        check_cancelled_opt(control)?;
        values.extend(
            buffer[..chunk_len]
                .as_chunks::<N>()
                .0
                .iter()
                .map(|chunk| from_le_bytes(*chunk)),
        );
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
        return Err(WsiError::invalid_slide(
            path,
            format!("DICOM basic offset table has non-u32 length {len}"),
        ));
    }
    if len > MAX_BASIC_OFFSET_TABLE_BYTES {
        return Err(WsiError::invalid_slide(
            path,
            format!(
                "DICOM basic offset table length {len} exceeds safety limit {MAX_BASIC_OFFSET_TABLE_BYTES}"
            ),
        ));
    }
    if let Some(number_of_frames) = number_of_frames {
        let expected_len = number_of_frames.checked_mul(4).ok_or_else(|| {
            WsiError::invalid_slide(path, "DICOM basic offset table length overflow")
        })?;
        if len != 0 && len != expected_len {
            return Err(WsiError::invalid_slide(
                path,
                format!(
                    "DICOM basic offset table length {len} does not match {number_of_frames} frames"
                ),
            ));
        }
    }
    usize::try_from(len / 4)
        .map_err(|_| WsiError::invalid_slide(path, "DICOM basic offset table length overflow"))
}
