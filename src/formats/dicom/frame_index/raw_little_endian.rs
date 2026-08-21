use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use crate::error::WsiError;

use super::model::{
    DicomEncapsulatedFrames, DicomExtendedOffsetTables, DicomFragmentRef, FastDicomFrameIndex,
};
use super::offset_tables::{
    checked_padded_fragment_len, read_basic_offset_table_at, read_extended_offset_tables_le,
};
use super::{
    check_index_control, DICOM_ITEM_TAG_LE, DICOM_SEQUENCE_DELIMITER_TAG_LE,
    EXPLICIT_VR_LONG_HEADER_LEN, EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE,
    EXTENDED_OFFSET_TABLE_TAG_LE, PIXEL_DATA_TAG_LE,
};
use crate::formats::dicom::metadata::invalid_slide;
use crate::formats::dicom::preflight::preflight_file_meta;

struct DicomEncapsulatedLayout {
    pixel_data_offset: u64,
    extended_offsets_value: Option<u64>,
    extended_lengths_value: Option<u64>,
}

#[cfg(test)]
pub(in super::super) fn scan_encapsulated_frames_raw_little_endian(
    path: &Path,
    number_of_frames: u32,
) -> Result<Option<DicomEncapsulatedFrames>, WsiError> {
    scan_encapsulated_frames_raw_little_endian_controlled(path, number_of_frames, None)
        .map(|index| index.map(|index| index.frames))
}

pub(super) fn scan_encapsulated_frames_raw_little_endian_controlled(
    path: &Path,
    number_of_frames: u32,
    control: Option<&crate::ReadControl>,
) -> Result<Option<FastDicomFrameIndex>, WsiError> {
    check_index_control(control)?;
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let expected_table_len = u64::from(number_of_frames)
        .checked_mul(8)
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| invalid_slide(path, "DICOM extended offset table is too large"))?;
    let Some(layout) = find_encapsulated_layout_le(&mut file, path, expected_table_len, control)?
    else {
        return Ok(None);
    };
    let pixel_data_offset = layout.pixel_data_offset;

    let extended_offset_tables = read_extended_offset_tables_le(
        &mut file,
        path,
        layout.extended_offsets_value,
        layout.extended_lengths_value,
        expected_table_len,
        control,
    )?;
    check_index_control(control)?;
    if let Some(tables) = extended_offset_tables.as_ref() {
        match build_single_fragment_index_from_extended_offsets(
            &mut file,
            path,
            pixel_data_offset,
            tables,
            number_of_frames,
            control,
        ) {
            Ok(index) => {
                return Ok(Some(FastDicomFrameIndex {
                    frames: index,
                    mapping: crate::DicomIndexMapping::ExtendedOffsetTableDirect,
                }));
            }
            Err(error) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %error,
                    "DICOM extended offsets do not describe one fragment per frame; scanning item headers"
                );
            }
        }
    }

    check_index_control(control)?;
    let file_len = file
        .metadata()
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?
        .len();
    let (fragments, offset_table) = scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
        &mut file,
        path,
        pixel_data_offset,
        file_len,
        Some(number_of_frames),
        control,
    )?;
    let index = super::offset_tables::build_encapsulated_frame_index_with_mapping(
        path,
        fragments,
        offset_table,
        extended_offset_tables.as_ref(),
        number_of_frames,
    )?;
    Ok(Some(index))
}

fn build_single_fragment_index_from_extended_offsets(
    file: &mut File,
    path: &Path,
    pixel_data_offset: u64,
    tables: &DicomExtendedOffsetTables,
    number_of_frames: u32,
    control: Option<&crate::ReadControl>,
) -> Result<DicomEncapsulatedFrames, WsiError> {
    check_index_control(control)?;
    super::offset_tables::validate_extended_offset_table_shape(path, tables, number_of_frames)?;
    let file_len = file
        .metadata()
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?
        .len();
    let offset_table_item = pixel_data_offset
        .checked_add(EXPLICIT_VR_LONG_HEADER_LEN as u64)
        .ok_or_else(|| invalid_slide(path, "DICOM Pixel Data offset overflow"))?;
    let mut item_header = [0u8; 8];
    read_exact_file_at_controlled(file, path, offset_table_item, &mut item_header, control)?;
    if item_header[0..4] != DICOM_ITEM_TAG_LE {
        return Err(invalid_slide(
            path,
            "DICOM Pixel Data is missing its basic offset table item",
        ));
    }
    let basic_offset_table_len = u32::from_le_bytes(
        item_header[4..8]
            .try_into()
            .expect("DICOM item length header is 4 bytes"),
    );
    if !basic_offset_table_len.is_multiple_of(4) {
        return Err(invalid_slide(
            path,
            "DICOM basic offset table length is not a multiple of four",
        ));
    }
    let first_fragment_item = offset_table_item
        .checked_add(8)
        .and_then(|offset| offset.checked_add(u64::from(basic_offset_table_len)))
        .ok_or_else(|| invalid_slide(path, "DICOM first fragment offset overflow"))?;

    let mut fragments = Vec::with_capacity(number_of_frames as usize);
    for (frame_index, (&offset, &declared_len)) in
        tables.offsets.iter().zip(&tables.lengths).enumerate()
    {
        check_index_control(control)?;
        let padded_len = checked_padded_fragment_len(path, frame_index, declared_len)?;
        let item_offset = first_fragment_item
            .checked_add(offset)
            .ok_or_else(|| invalid_slide(path, "DICOM extended frame offset overflow"))?;
        let payload_offset = item_offset
            .checked_add(8)
            .ok_or_else(|| invalid_slide(path, "DICOM fragment payload offset overflow"))?;
        let fragment_end = payload_offset
            .checked_add(u64::from(padded_len))
            .ok_or_else(|| invalid_slide(path, "DICOM fragment end offset overflow"))?;
        if fragment_end > file_len {
            return Err(invalid_slide(
                path,
                format!("DICOM frame {frame_index} extends beyond the source file"),
            ));
        }
        if let Some(next_offset) = tables.offsets.get(frame_index + 1) {
            let next_item = first_fragment_item
                .checked_add(*next_offset)
                .ok_or_else(|| invalid_slide(path, "DICOM next frame offset overflow"))?;
            if fragment_end != next_item {
                return Err(invalid_slide(
                    path,
                    format!(
                        "DICOM frame {frame_index} spans multiple fragments or has inconsistent extended length"
                    ),
                ));
            }
        }
        fragments.push(DicomFragmentRef {
            payload_offset,
            item_offset,
            len: padded_len,
        });
    }

    validate_extended_item_header_sample(file, path, &fragments, control)?;

    let last = fragments
        .last()
        .ok_or_else(|| invalid_slide(path, "DICOM extended offset table is empty"))?;
    let delimiter_offset = last
        .payload_offset
        .checked_add(u64::from(last.len))
        .ok_or_else(|| invalid_slide(path, "DICOM pixel sequence delimiter offset overflow"))?;
    read_exact_file_at_controlled(file, path, delimiter_offset, &mut item_header, control)?;
    if item_header[0..4] != DICOM_SEQUENCE_DELIMITER_TAG_LE || item_header[4..8] != [0; 4] {
        return Err(invalid_slide(
            path,
            "DICOM final fragment is not followed by a valid sequence delimiter",
        ));
    }

    Ok(DicomEncapsulatedFrames {
        frame_ranges: (0..fragments.len()).map(|index| index..index + 1).collect(),
        fragments,
    })
}

fn validate_extended_item_header_sample(
    file: &File,
    path: &Path,
    fragments: &[DicomFragmentRef],
    control: Option<&crate::ReadControl>,
) -> Result<(), WsiError> {
    // Exhaustively touching every compressed-frame page makes preparation scale with the
    // complete slide payload. The index shape and every byte range were validated above; sample
    // large indexes here, then validate each selected Item header again at extraction time before
    // any compressed bytes are returned to a codec.
    const FULL_VALIDATION_FRAME_LIMIT: usize = 1_024;
    const LARGE_INDEX_SAMPLE_COUNT: usize = 64;
    let validate = |frame_index: usize| -> Result<(), WsiError> {
        let fragment = &fragments[frame_index];
        let mut header = [0u8; 8];
        read_exact_file_at_controlled(file, path, fragment.item_offset, &mut header, control)?;
        let item_len = u32::from_le_bytes(
            header[4..8]
                .try_into()
                .expect("DICOM item length header is 4 bytes"),
        );
        if header[0..4] != DICOM_ITEM_TAG_LE || item_len != fragment.len {
            return Err(invalid_slide(
                path,
                format!("DICOM frame {frame_index} Item header does not match its extended length"),
            ));
        }
        Ok(())
    };
    if fragments.len() <= FULL_VALIDATION_FRAME_LIMIT {
        for frame_index in 0..fragments.len() {
            validate(frame_index)?;
        }
        return Ok(());
    }
    let last = fragments.len() - 1;
    for sample in 0..LARGE_INDEX_SAMPLE_COUNT {
        validate(sample * last / (LARGE_INDEX_SAMPLE_COUNT - 1))?;
    }
    Ok(())
}

fn find_encapsulated_layout_le(
    file: &mut File,
    path: &Path,
    expected_table_len: u32,
    control: Option<&crate::ReadControl>,
) -> Result<Option<DicomEncapsulatedLayout>, WsiError> {
    check_index_control(control)?;
    position_at_explicit_little_endian_dataset(file, path)?;
    let file_len = file
        .metadata()
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?
        .len();
    let mut cursor = file
        .stream_position()
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let mut reader = HeaderWindowReader::new(file, path, file_len, control);
    let mut extended_offsets_value = None;
    let mut extended_lengths_value = None;

    while cursor < file_len {
        check_index_control(control)?;
        let header = read_explicit_le_element_header(&mut reader, path, cursor, file_len)?;
        if header.tag == PIXEL_DATA_TAG_LE {
            if header.length != u32::MAX || !matches!(&header.vr, b"OB" | b"OW" | b"UN") {
                return Err(invalid_slide(
                    path,
                    "DICOM Pixel Data is not an encapsulated explicit-VR element",
                ));
            }
            return Ok(Some(DicomEncapsulatedLayout {
                pixel_data_offset: cursor,
                extended_offsets_value,
                extended_lengths_value,
            }));
        }
        if header.vr == *b"OV" && header.length == expected_table_len {
            if header.tag == EXTENDED_OFFSET_TABLE_TAG_LE {
                extended_offsets_value = Some(header.value_offset);
            } else if header.tag == EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE {
                extended_lengths_value = Some(header.value_offset);
            }
        }
        cursor = skip_explicit_le_value(&mut reader, path, &header, file_len, control)?;
    }
    Ok(None)
}

struct HeaderWindowReader<'a> {
    file: &'a File,
    path: &'a Path,
    file_len: u64,
    control: Option<&'a crate::ReadControl>,
    window_start: u64,
    window: Vec<u8>,
}

impl<'a> HeaderWindowReader<'a> {
    const WINDOW_BYTES: usize = 64 * 1024;

    fn new(
        file: &'a File,
        path: &'a Path,
        file_len: u64,
        control: Option<&'a crate::ReadControl>,
    ) -> Self {
        Self {
            file,
            path,
            file_len,
            control,
            window_start: 0,
            window: Vec::new(),
        }
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), WsiError> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| invalid_slide(self.path, "DICOM header read offset overflow"))?;
        if end > self.file_len {
            return Err(invalid_slide(
                self.path,
                "DICOM header read extends beyond the source file",
            ));
        }
        let window_end = self.window_start + self.window.len() as u64;
        if offset < self.window_start || end > window_end {
            let remaining = usize::try_from(self.file_len - offset)
                .unwrap_or(usize::MAX)
                .min(Self::WINDOW_BYTES);
            self.window.resize(remaining, 0);
            read_exact_file_at_controlled(
                self.file,
                self.path,
                offset,
                &mut self.window,
                self.control,
            )?;
            self.window_start = offset;
        }
        let relative_start = usize::try_from(offset - self.window_start)
            .expect("bounded DICOM header window offset fits usize");
        let relative_end = relative_start + buffer.len();
        buffer.copy_from_slice(&self.window[relative_start..relative_end]);
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ExplicitLeElementHeader {
    tag: [u8; 4],
    vr: [u8; 2],
    length: u32,
    value_offset: u64,
}

fn read_explicit_le_element_header(
    reader: &mut HeaderWindowReader<'_>,
    path: &Path,
    offset: u64,
    file_len: u64,
) -> Result<ExplicitLeElementHeader, WsiError> {
    let header_end = offset
        .checked_add(8)
        .ok_or_else(|| invalid_slide(path, "DICOM element header offset overflow"))?;
    if header_end > file_len {
        return Err(invalid_slide(
            path,
            "DICOM element header extends beyond the source file",
        ));
    }
    let mut prefix = [0u8; 8];
    reader.read_exact_at(offset, &mut prefix)?;
    let tag: [u8; 4] = prefix[0..4]
        .try_into()
        .expect("DICOM tag header is four bytes");
    let vr: [u8; 2] = prefix[4..6]
        .try_into()
        .expect("DICOM VR header is two bytes");
    if !vr.iter().all(u8::is_ascii_uppercase) {
        return Err(invalid_slide(
            path,
            format!("invalid explicit-VR DICOM header at byte {offset}"),
        ));
    }
    let long_vr = matches!(
        &vr,
        b"OB" | b"OD" | b"OF" | b"OL" | b"OV" | b"OW" | b"SQ" | b"UC" | b"UR" | b"UT" | b"UN"
    );
    let (length, value_offset) = if long_vr {
        if prefix[6..8] != [0, 0] {
            return Err(invalid_slide(
                path,
                format!("invalid reserved bytes in DICOM header at byte {offset}"),
            ));
        }
        let value_offset = offset
            .checked_add(EXPLICIT_VR_LONG_HEADER_LEN as u64)
            .ok_or_else(|| invalid_slide(path, "DICOM value offset overflow"))?;
        if value_offset > file_len {
            return Err(invalid_slide(
                path,
                "DICOM long element header exceeds the source file",
            ));
        }
        let mut length = [0u8; 4];
        reader.read_exact_at(header_end, &mut length)?;
        (u32::from_le_bytes(length), value_offset)
    } else {
        (
            u32::from(u16::from_le_bytes(
                prefix[6..8]
                    .try_into()
                    .expect("DICOM short length is two bytes"),
            )),
            header_end,
        )
    };
    Ok(ExplicitLeElementHeader {
        tag,
        vr,
        length,
        value_offset,
    })
}

fn skip_explicit_le_value(
    reader: &mut HeaderWindowReader<'_>,
    path: &Path,
    header: &ExplicitLeElementHeader,
    file_len: u64,
    control: Option<&crate::ReadControl>,
) -> Result<u64, WsiError> {
    if header.length == u32::MAX {
        return skip_undefined_explicit_le_sequence(
            reader,
            path,
            header.value_offset,
            file_len,
            control,
        );
    }
    checked_value_end(path, header.value_offset, header.length, file_len)
}

fn skip_undefined_explicit_le_sequence(
    reader: &mut HeaderWindowReader<'_>,
    path: &Path,
    mut cursor: u64,
    file_len: u64,
    control: Option<&crate::ReadControl>,
) -> Result<u64, WsiError> {
    loop {
        check_index_control(control)?;
        let (tag, length, value_offset) = read_item_header(reader, path, cursor, file_len)?;
        if tag == DICOM_SEQUENCE_DELIMITER_TAG_LE {
            if length != 0 {
                return Err(invalid_slide(
                    path,
                    "DICOM sequence delimiter has non-zero length",
                ));
            }
            return Ok(value_offset);
        }
        if tag != DICOM_ITEM_TAG_LE {
            return Err(invalid_slide(
                path,
                format!(
                    "unexpected DICOM sequence tag {:02x?} at byte {cursor}",
                    tag
                ),
            ));
        }
        cursor = if length == u32::MAX {
            skip_undefined_explicit_le_item(reader, path, value_offset, file_len, control)?
        } else {
            checked_value_end(path, value_offset, length, file_len)?
        };
    }
}

fn skip_undefined_explicit_le_item(
    reader: &mut HeaderWindowReader<'_>,
    path: &Path,
    mut cursor: u64,
    file_len: u64,
    control: Option<&crate::ReadControl>,
) -> Result<u64, WsiError> {
    loop {
        check_index_control(control)?;
        let mut tag_bytes = [0u8; 4];
        reader.read_exact_at(cursor, &mut tag_bytes)?;
        if tag_bytes == [0xFE, 0xFF, 0x0D, 0xE0] {
            let (tag, length, value_offset) = read_item_header(reader, path, cursor, file_len)?;
            debug_assert_eq!(tag, [0xFE, 0xFF, 0x0D, 0xE0]);
            if length != 0 {
                return Err(invalid_slide(
                    path,
                    "DICOM item delimiter has non-zero length",
                ));
            }
            return Ok(value_offset);
        }
        let header = read_explicit_le_element_header(reader, path, cursor, file_len)?;
        cursor = skip_explicit_le_value(reader, path, &header, file_len, control)?;
    }
}

fn read_item_header(
    reader: &mut HeaderWindowReader<'_>,
    path: &Path,
    offset: u64,
    file_len: u64,
) -> Result<([u8; 4], u32, u64), WsiError> {
    let value_offset = offset
        .checked_add(8)
        .ok_or_else(|| invalid_slide(path, "DICOM item header offset overflow"))?;
    if value_offset > file_len {
        return Err(invalid_slide(
            path,
            "DICOM item header extends beyond the source file",
        ));
    }
    let mut header = [0u8; 8];
    reader.read_exact_at(offset, &mut header)?;
    Ok((
        header[0..4]
            .try_into()
            .expect("DICOM item tag is four bytes"),
        u32::from_le_bytes(
            header[4..8]
                .try_into()
                .expect("DICOM item length is four bytes"),
        ),
        value_offset,
    ))
}

fn checked_value_end(
    path: &Path,
    value_offset: u64,
    length: u32,
    file_len: u64,
) -> Result<u64, WsiError> {
    let end = value_offset
        .checked_add(u64::from(length))
        .ok_or_else(|| invalid_slide(path, "DICOM element value offset overflow"))?;
    if end > file_len {
        return Err(invalid_slide(
            path,
            "DICOM element value extends beyond the source file",
        ));
    }
    Ok(end)
}

fn position_at_explicit_little_endian_dataset(
    file: &mut File,
    path: &Path,
) -> Result<(), WsiError> {
    if preflight_file_meta(file, path).is_ok() {
        return Ok(());
    }

    // The raw scanner is also exercised against header-only synthetic inputs
    // which intentionally omit a complete File Meta Information group. Keep
    // that low-level compatibility path bounded by positioning directly after
    // a valid preamble instead of invoking the allocating metadata parser.
    file.seek(SeekFrom::Start(128))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let dataset_offset = if &magic == b"DICM" { 132 } else { 0 };
    file.seek(SeekFrom::Start(dataset_offset))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    Ok(())
}

pub(in super::super) fn scan_raw_encapsulated_pixel_sequence_with_reader_controlled<
    R: Read + Seek,
>(
    reader: &mut R,
    path: &Path,
    pixel_data_offset: u64,
    file_len: u64,
    number_of_frames: Option<u32>,
    control: Option<&crate::ReadControl>,
) -> Result<(Vec<DicomFragmentRef>, Vec<u32>), WsiError> {
    check_index_control(control)?;
    let mut cursor = pixel_data_offset
        .checked_add(EXPLICIT_VR_LONG_HEADER_LEN as u64)
        .ok_or_else(|| invalid_slide(path, "DICOM raw Pixel Data offset overflow"))?;
    let mut offset_table = None;
    let mut fragments = Vec::new();

    loop {
        check_index_control(control)?;
        let mut item_header = [0u8; 8];
        read_exact_at(reader, path, cursor, &mut item_header)?;
        let tag = &item_header[0..4];
        let len = u32::from_le_bytes(
            item_header[4..8]
                .try_into()
                .expect("DICOM item length header is 4 bytes"),
        );
        cursor = cursor
            .checked_add(item_header.len() as u64)
            .ok_or_else(|| invalid_slide(path, "DICOM raw item offset overflow"))?;

        if tag == DICOM_SEQUENCE_DELIMITER_TAG_LE {
            if len != 0 {
                return Err(invalid_slide(
                    path,
                    format!("DICOM pixel sequence delimiter has non-zero length {len}"),
                ));
            }
            check_index_control(control)?;
            return Ok((fragments, offset_table.unwrap_or_default()));
        }
        if tag != DICOM_ITEM_TAG_LE {
            return Err(invalid_slide(
                path,
                format!(
                    "unexpected DICOM pixel sequence tag {:02x?} at byte {}",
                    tag,
                    cursor - item_header.len() as u64
                ),
            ));
        }

        let payload_end = cursor
            .checked_add(u64::from(len))
            .ok_or_else(|| invalid_slide(path, "DICOM raw item payload offset overflow"))?;
        if payload_end > file_len {
            return Err(invalid_slide(
                path,
                "DICOM pixel sequence item extends beyond the source file",
            ));
        }

        if offset_table.is_none() {
            offset_table = Some(read_basic_offset_table_at(
                reader,
                path,
                cursor,
                len,
                number_of_frames,
                control,
            )?);
        } else {
            if len == 0 || len == u32::MAX {
                return Err(invalid_slide(
                    path,
                    "zero or undefined-length DICOM pixel fragment is not supported",
                ));
            }
            fragments.push(DicomFragmentRef {
                payload_offset: cursor,
                item_offset: cursor - item_header.len() as u64,
                len,
            });
        }

        cursor = payload_end;
    }
}

pub(in super::super) fn read_exact_at(
    file: &mut (impl Read + Seek),
    path: &Path,
    offset: u64,
    buf: &mut [u8],
) -> Result<(), WsiError> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    file.read_exact(buf).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })
}

fn read_exact_file_at_controlled(
    file: &File,
    path: &Path,
    mut offset: u64,
    mut buffer: &mut [u8],
    control: Option<&crate::ReadControl>,
) -> Result<(), WsiError> {
    while !buffer.is_empty() {
        check_index_control(control)?;
        let read = read_file_at(file, buffer, offset).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
        if read == 0 {
            return Err(WsiError::IoWithPath {
                source: Arc::new(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "failed to fill DICOM positional read buffer",
                )),
                path: path.to_path_buf(),
            });
        }
        offset = offset
            .checked_add(read as u64)
            .ok_or_else(|| invalid_slide(path, "DICOM positional read offset overflow"))?;
        buffer = &mut buffer[read..];
    }
    check_index_control(control)
}

#[cfg(unix)]
fn read_file_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buffer, offset)
}

#[cfg(windows)]
fn read_file_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buffer, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_file_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    file.read(buffer)
}
