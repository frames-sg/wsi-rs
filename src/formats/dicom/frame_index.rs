use super::*;

#[derive(Debug)]
pub(super) struct DicomFrameReadSpan {
    pub(super) frame_index: u32,
    pub(super) frame_range: std::ops::Range<usize>,
    pub(super) start: u64,
    pub(super) end: u64,
}

#[derive(Debug)]
pub(super) struct DicomFrameReadGroup {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) spans: Vec<DicomFrameReadSpan>,
}

pub(super) fn group_frame_read_spans(
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

pub(super) fn copy_fragments_from_window(
    path: &Path,
    window_start: u64,
    window: &[u8],
    fragments: &[DicomFragmentRef],
) -> Result<Vec<u8>, WsiError> {
    let total_len: usize = fragments.iter().map(|fragment| fragment.len as usize).sum();
    let mut data = Vec::with_capacity(total_len);
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

pub(super) fn reopen_dicom_object(path: &Path) -> Result<DefaultDicomObject, WsiError> {
    dicom_object::open_file(path).map_err(|source| WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: format!("failed to reopen DICOM object: {source}"),
    })
}

pub(super) fn scan_encapsulated_frames(
    path: &Path,
    transfer_syntax_uid: &str,
    number_of_frames: u32,
) -> Result<DicomEncapsulatedFrames, WsiError> {
    let transfer_syntax = TransferSyntaxRegistry
        .get(transfer_syntax_uid)
        .or_else(|| {
            JP2K_TRANSFER_SYNTAXES
                .contains(&transfer_syntax_uid)
                .then(|| TransferSyntaxRegistry.get(uids::EXPLICIT_VR_LITTLE_ENDIAN))
                .flatten()
        })
        .ok_or_else(|| {
            invalid_slide(
                path,
                format!("unknown transfer syntax {transfer_syntax_uid}"),
            )
        })?;
    let mut reader = BufReader::new(File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?);
    position_reader_for_dicom_magic(&mut reader, path)?;
    let _meta = FileMetaTable::from_reader(&mut reader)
        .map_err(|source| invalid_slide(path, format!("cannot parse DICOM file meta: {source}")))?;
    let mut tokens = LazyDataSetReader::new_with_ts(reader, transfer_syntax)
        .map_err(|source| invalid_slide(path, format!("cannot stream DICOM dataset: {source}")))?;

    let mut in_pixel_sequence = false;
    let mut awaiting_offset_table = false;
    let mut offset_table = Vec::new();
    let mut fragments = Vec::new();

    while let Some(token) = tokens.advance() {
        let token = token
            .map_err(|source| invalid_slide(path, format!("cannot read DICOM token: {source}")))?;
        match token {
            LazyDataToken::PixelSequenceStart => {
                in_pixel_sequence = true;
                awaiting_offset_table = true;
            }
            LazyDataToken::ItemStart { len }
                if in_pixel_sequence && awaiting_offset_table && len.0 == 0 =>
            {
                awaiting_offset_table = false;
            }
            LazyDataToken::LazyItemValue { len, decoder }
                if in_pixel_sequence && awaiting_offset_table =>
            {
                decoder
                    .read_u32_to_vec(len, &mut offset_table)
                    .map_err(|source| {
                        invalid_slide(
                            path,
                            format!("cannot read DICOM basic offset table: {source}"),
                        )
                    })?;
                awaiting_offset_table = false;
            }
            LazyDataToken::LazyItemValue { len, decoder } if in_pixel_sequence => {
                let payload_offset = decoder.position();
                let item_offset = payload_offset.saturating_sub(8);
                decoder.skip_bytes(len).map_err(|source| {
                    invalid_slide(path, format!("cannot skip DICOM fragment: {source}"))
                })?;
                fragments.push(DicomFragmentRef {
                    payload_offset,
                    item_offset,
                    len,
                });
            }
            LazyDataToken::ItemStart { len } if in_pixel_sequence && len.0 == 0 => {
                return Err(invalid_slide(
                    path,
                    "zero-length DICOM pixel fragment is not supported",
                ));
            }
            LazyDataToken::SequenceEnd if in_pixel_sequence => break,
            other => {
                other.skip().map_err(|source| {
                    invalid_slide(path, format!("cannot skip DICOM token: {source}"))
                })?;
            }
        }
    }

    if fragments.is_empty() {
        if let Some(frames) = scan_encapsulated_frames_raw_little_endian(path, number_of_frames)? {
            return Ok(frames);
        }
    }

    build_encapsulated_frame_index(path, fragments, offset_table, number_of_frames)
}

pub(super) const PIXEL_DATA_TAG_LE: [u8; 4] = [0xE0, 0x7F, 0x10, 0x00];
pub(super) const DICOM_ITEM_TAG_LE: [u8; 4] = [0xFE, 0xFF, 0x00, 0xE0];
pub(super) const DICOM_SEQUENCE_DELIMITER_TAG_LE: [u8; 4] = [0xFE, 0xFF, 0xDD, 0xE0];
pub(super) const UNDEFINED_LENGTH_LE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
pub(super) const EXPLICIT_VR_LONG_HEADER_LEN: usize = 12;

pub(super) fn scan_encapsulated_frames_raw_little_endian(
    path: &Path,
    number_of_frames: u32,
) -> Result<Option<DicomEncapsulatedFrames>, WsiError> {
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let Some(pixel_data_offset) = find_encapsulated_pixel_data_offset_le(&mut file, path)? else {
        return Ok(None);
    };

    let (fragments, offset_table) =
        scan_raw_encapsulated_pixel_sequence(&mut file, path, pixel_data_offset)?;
    build_encapsulated_frame_index(path, fragments, offset_table, number_of_frames).map(Some)
}

pub(super) fn find_encapsulated_pixel_data_offset_le(
    file: &mut File,
    path: &Path,
) -> Result<Option<u64>, WsiError> {
    const CHUNK_LEN: usize = 64 * 1024;
    let mut chunk = [0u8; CHUNK_LEN];
    let mut overlap = Vec::new();
    let mut chunk_offset = 0u64;

    file.seek(SeekFrom::Start(0))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;

    loop {
        let read_len = file
            .read(&mut chunk)
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
        if read_len == 0 {
            return Ok(None);
        }

        let window_offset = chunk_offset.saturating_sub(overlap.len() as u64);
        let mut window = Vec::with_capacity(overlap.len() + read_len);
        window.extend_from_slice(&overlap);
        window.extend_from_slice(&chunk[..read_len]);

        for index in 0..=window.len().saturating_sub(EXPLICIT_VR_LONG_HEADER_LEN) {
            let header = &window[index..index + EXPLICIT_VR_LONG_HEADER_LEN];
            if is_encapsulated_pixel_data_header_le(header) {
                return Ok(Some(window_offset + index as u64));
            }
        }

        let keep = window.len().min(EXPLICIT_VR_LONG_HEADER_LEN - 1);
        overlap.clear();
        overlap.extend_from_slice(&window[window.len() - keep..]);
        chunk_offset = chunk_offset
            .checked_add(read_len as u64)
            .ok_or_else(|| invalid_slide(path, "DICOM raw Pixel Data scan offset overflow"))?;
    }
}

pub(super) fn is_encapsulated_pixel_data_header_le(header: &[u8]) -> bool {
    header.len() >= EXPLICIT_VR_LONG_HEADER_LEN
        && header[0..4] == PIXEL_DATA_TAG_LE
        && matches!(&header[4..6], b"OB" | b"OW" | b"UN")
        && header[6..8] == [0, 0]
        && header[8..12] == UNDEFINED_LENGTH_LE
}

pub(super) fn scan_raw_encapsulated_pixel_sequence(
    file: &mut File,
    path: &Path,
    pixel_data_offset: u64,
) -> Result<(Vec<DicomFragmentRef>, Vec<u32>), WsiError> {
    let mut cursor = pixel_data_offset
        .checked_add(EXPLICIT_VR_LONG_HEADER_LEN as u64)
        .ok_or_else(|| invalid_slide(path, "DICOM raw Pixel Data offset overflow"))?;
    let mut offset_table = None;
    let mut fragments = Vec::new();

    loop {
        let mut item_header = [0u8; 8];
        read_exact_at(file, path, cursor, &mut item_header)?;
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

        if offset_table.is_none() {
            offset_table = Some(read_basic_offset_table_at(file, path, cursor, len)?);
        } else {
            if len == 0 {
                return Err(invalid_slide(
                    path,
                    "zero-length DICOM pixel fragment is not supported",
                ));
            }
            fragments.push(DicomFragmentRef {
                payload_offset: cursor,
                item_offset: cursor - item_header.len() as u64,
                len,
            });
        }

        cursor = cursor
            .checked_add(len as u64)
            .ok_or_else(|| invalid_slide(path, "DICOM raw item payload offset overflow"))?;
    }
}

pub(super) fn read_basic_offset_table_at(
    file: &mut File,
    path: &Path,
    offset: u64,
    len: u32,
) -> Result<Vec<u32>, WsiError> {
    if !len.is_multiple_of(4) {
        return Err(invalid_slide(
            path,
            format!("DICOM basic offset table has non-u32 length {len}"),
        ));
    }
    let len = usize::try_from(len)
        .map_err(|_| invalid_slide(path, "DICOM basic offset table length overflow"))?;
    let mut bytes = vec![0u8; len];
    read_exact_at(file, path, offset, &mut bytes)?;
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| {
            u32::from_le_bytes(
                chunk
                    .try_into()
                    .expect("DICOM basic offset table chunk is 4 bytes"),
            )
        })
        .collect())
}

pub(super) fn read_exact_at(
    file: &mut File,
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

pub(super) fn build_encapsulated_frame_index(
    path: &Path,
    fragments: Vec<DicomFragmentRef>,
    offset_table: Vec<u32>,
    number_of_frames: u32,
) -> Result<DicomEncapsulatedFrames, WsiError> {
    if number_of_frames == 0 {
        return Err(invalid_slide(path, "DICOM reported zero frames"));
    }
    if fragments.is_empty() {
        return Err(invalid_slide(
            path,
            "DICOM encapsulated pixel data has no fragments",
        ));
    }

    let frame_ranges = if number_of_frames == 1 {
        std::iter::once(0..fragments.len()).collect()
    } else if !offset_table.is_empty() {
        let base_item_offset = fragments[0].item_offset;
        let fragment_indices_by_offset: HashMap<u64, usize> = fragments
            .iter()
            .enumerate()
            .map(|(index, fragment)| (fragment.item_offset, index))
            .collect();
        let mut start_indices = Vec::with_capacity(offset_table.len());
        for offset in &offset_table {
            let target = base_item_offset + *offset as u64;
            let index = fragment_indices_by_offset
                .get(&target)
                .copied()
                .ok_or_else(|| {
                    invalid_slide(
                        path,
                        format!(
                            "DICOM basic offset table points to missing fragment offset {offset}"
                        ),
                    )
                })?;
            start_indices.push(index);
        }
        if start_indices.len() != number_of_frames as usize {
            return Err(invalid_slide(
                path,
                format!(
                    "DICOM basic offset table length {} does not match number_of_frames {}",
                    start_indices.len(),
                    number_of_frames
                ),
            ));
        }
        let mut ranges = Vec::with_capacity(start_indices.len());
        for (frame, start) in start_indices.iter().copied().enumerate() {
            let end = start_indices
                .get(frame + 1)
                .copied()
                .unwrap_or(fragments.len());
            ranges.push(start..end);
        }
        ranges
    } else if fragments.len() == number_of_frames as usize {
        (0..fragments.len()).map(|index| index..index + 1).collect()
    } else {
        return Err(invalid_slide(
            path,
            format!(
                "cannot map {} DICOM fragments to {} frames without a basic offset table",
                fragments.len(),
                number_of_frames
            ),
        ));
    };

    Ok(DicomEncapsulatedFrames {
        fragments,
        frame_ranges,
    })
}

pub(super) fn position_reader_for_dicom_magic<R: Read + Seek>(
    reader: &mut R,
    path: &Path,
) -> Result<(), WsiError> {
    let mut preamble = [0u8; 132];
    reader
        .read_exact(&mut preamble)
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let start = if &preamble[128..] == b"DICM" { 128 } else { 0 };
    reader
        .seek(SeekFrom::Start(start))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    Ok(())
}
