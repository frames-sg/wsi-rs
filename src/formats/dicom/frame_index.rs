use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) struct CompressedFramePreflight {
    pub(super) total_len: usize,
}

/// Validate one compressed frame's complete fragment graph before any payload
/// buffer is reserved, copied, or passed to a decoder.
pub(super) fn preflight_compressed_frame(
    path: &Path,
    fragments: &[DicomFragmentRef],
) -> Result<CompressedFramePreflight, WsiError> {
    if fragments.is_empty() {
        return Err(invalid_slide(
            path,
            "compressed DICOM frame has no fragments",
        ));
    }
    let mut total = 0u64;
    for fragment in fragments {
        let expected_payload_offset = fragment
            .item_offset
            .checked_add(8)
            .ok_or_else(|| invalid_slide(path, "DICOM fragment Item offset overflow"))?;
        if expected_payload_offset != fragment.payload_offset {
            return Err(invalid_slide(
                path,
                "DICOM fragment payload offset does not follow its Item header",
            ));
        }
        fragment
            .payload_offset
            .checked_add(u64::from(fragment.len))
            .ok_or_else(|| invalid_slide(path, "DICOM fragment payload offset overflow"))?;
        let fragment_len = u64::from(fragment.len);
        if fragment_len > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
            return Err(WsiError::ResourceLimit {
                resource: "compressed DICOM frame",
                requested: fragment_len,
                limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
            });
        }
        total = total
            .checked_add(fragment_len)
            .ok_or_else(|| invalid_slide(path, "DICOM compressed frame length overflow"))?;
        if total > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
            return Err(WsiError::ResourceLimit {
                resource: "compressed DICOM frame",
                requested: total,
                limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
            });
        }
    }
    let total_len = usize::try_from(total)
        .map_err(|_| invalid_slide(path, "DICOM compressed frame is not addressable"))?;
    Ok(CompressedFramePreflight { total_len })
}

pub(super) fn preflight_compressed_lengths(
    path: &Path,
    lengths: impl IntoIterator<Item = usize>,
) -> Result<usize, WsiError> {
    let mut total = 0u64;
    for length in lengths {
        let length = u64::try_from(length)
            .map_err(|_| invalid_slide(path, "DICOM compressed fragment length overflow"))?;
        if length > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
            return Err(WsiError::ResourceLimit {
                resource: "compressed DICOM frame",
                requested: length,
                limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
            });
        }
        total = total
            .checked_add(length)
            .ok_or_else(|| invalid_slide(path, "DICOM compressed frame length overflow"))?;
        if total > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
            return Err(WsiError::ResourceLimit {
                resource: "compressed DICOM frame",
                requested: total,
                limit: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES,
            });
        }
    }
    usize::try_from(total)
        .map_err(|_| invalid_slide(path, "DICOM compressed frame is not addressable"))
}

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
    let preflight = preflight_compressed_frame(path, fragments)?;
    let mut data = Vec::new();
    data.try_reserve_exact(preflight.total_len)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "compressed DICOM frame",
            requested: preflight.total_len as u64,
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

pub(super) fn reopen_dicom_object(path: &Path) -> Result<DefaultDicomObject, WsiError> {
    dicom_object::open_file(path).map_err(|source| WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: format!("failed to reopen DICOM object: {source}"),
    })
}

pub(super) fn scan_encapsulated_frames_controlled(
    path: &Path,
    transfer_syntax_uid: &str,
    number_of_frames: u32,
    control: Option<&crate::ReadControl>,
) -> Result<DicomEncapsulatedFrames, WsiError> {
    check_index_control(control)?;
    let fast_started = index_diagnostic_timer(control);
    match scan_encapsulated_frames_raw_little_endian_controlled(path, number_of_frames, control) {
        Ok(Some(index)) => {
            let elapsed = index_elapsed(fast_started);
            record_index_diagnostic(
                control,
                crate::DicomIndexOutcome::BuiltFast {
                    mapping: index.mapping,
                },
                elapsed,
            );
            if let Some(elapsed) = elapsed {
                tracing::debug!(
                    path = %path.display(),
                    strategy = ?index.mapping,
                    elapsed_us = elapsed.as_micros(),
                    "built DICOM encapsulated frame index"
                );
            }
            return Ok(index.frames);
        }
        Ok(None) => {
            let elapsed = index_elapsed(fast_started);
            record_index_diagnostic(control, crate::DicomIndexOutcome::FastPathFallback, elapsed);
        }
        Err(WsiError::Cancelled) => return Err(WsiError::Cancelled),
        Err(error) => {
            let elapsed = index_elapsed(fast_started);
            record_index_diagnostic(control, crate::DicomIndexOutcome::FastPathFallback, elapsed);
            tracing::debug!(
                path = %path.display(),
                error = %error,
                "fast DICOM encapsulated frame indexing failed; using token parser"
            );
        }
    }

    check_index_control(control)?;
    let token_started = index_diagnostic_timer(control);
    let frames =
        scan_encapsulated_frames_tokenized(path, transfer_syntax_uid, number_of_frames, control)?;
    let elapsed = index_elapsed(token_started);
    record_index_diagnostic(control, crate::DicomIndexOutcome::TokenFallback, elapsed);
    if let Some(elapsed) = elapsed {
        tracing::debug!(
            path = %path.display(),
            strategy = "token_fallback",
            elapsed_us = elapsed.as_micros(),
            "built DICOM encapsulated frame index"
        );
    }
    Ok(frames)
}

fn index_diagnostic_timer(control: Option<&crate::ReadControl>) -> Option<std::time::Instant> {
    index_diagnostic_timer_with(
        control,
        tracing::enabled!(tracing::Level::DEBUG),
        std::time::Instant::now,
    )
}

pub(super) fn index_diagnostic_timer_with(
    control: Option<&crate::ReadControl>,
    tracing_enabled: bool,
    clock: impl FnOnce() -> std::time::Instant,
) -> Option<std::time::Instant> {
    (control.is_some_and(crate::ReadControl::diagnostics_enabled) || tracing_enabled).then(clock)
}

fn index_elapsed(started: Option<std::time::Instant>) -> Option<std::time::Duration> {
    started.map(|started| started.elapsed())
}

fn record_index_diagnostic(
    control: Option<&crate::ReadControl>,
    outcome: crate::DicomIndexOutcome,
    elapsed: Option<std::time::Duration>,
) {
    if let (Some(control), Some(elapsed)) = (control, elapsed) {
        control.record_diagnostic(crate::ReadDiagnostic::DicomIndex(
            crate::DicomIndexDiagnostic::new(outcome, elapsed),
        ));
    }
}

fn scan_encapsulated_frames_tokenized(
    path: &Path,
    transfer_syntax_uid: &str,
    number_of_frames: u32,
    control: Option<&crate::ReadControl>,
) -> Result<DicomEncapsulatedFrames, WsiError> {
    check_index_control(control)?;
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
        check_index_control(control)?;
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
                check_index_control(control)?;
                let entry_count =
                    validate_basic_offset_table_len(path, len, Some(number_of_frames))?;
                offset_table
                    .try_reserve_exact(entry_count)
                    .map_err(|_| invalid_slide(path, "cannot allocate DICOM basic offset table"))?;
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
                check_index_control(control)?;
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

    check_index_control(control)?;
    build_encapsulated_frame_index(path, fragments, offset_table, number_of_frames)
}

fn check_index_control(control: Option<&crate::ReadControl>) -> Result<(), WsiError> {
    control.map_or(Ok(()), crate::ReadControl::check_cancelled)
}

pub(super) const PIXEL_DATA_TAG_LE: [u8; 4] = [0xE0, 0x7F, 0x10, 0x00];
pub(super) const EXTENDED_OFFSET_TABLE_TAG_LE: [u8; 4] = [0xE0, 0x7F, 0x01, 0x00];
pub(super) const EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE: [u8; 4] = [0xE0, 0x7F, 0x02, 0x00];
pub(super) const DICOM_ITEM_TAG_LE: [u8; 4] = [0xFE, 0xFF, 0x00, 0xE0];
pub(super) const DICOM_SEQUENCE_DELIMITER_TAG_LE: [u8; 4] = [0xFE, 0xFF, 0xDD, 0xE0];
#[cfg(test)]
pub(super) const UNDEFINED_LENGTH_LE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
pub(super) const EXPLICIT_VR_LONG_HEADER_LEN: usize = 12;
// Enough for 16,777,216 frames while preventing untrusted metadata from
// turning one index build into a multi-gigabyte allocation.
const MAX_BASIC_OFFSET_TABLE_BYTES: u32 = 64 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct DicomExtendedOffsetTables {
    pub(super) offsets: Vec<u64>,
    pub(super) lengths: Vec<u64>,
}

struct DicomEncapsulatedLayout {
    pixel_data_offset: u64,
    extended_offsets_value: Option<u64>,
    extended_lengths_value: Option<u64>,
}

struct FastDicomFrameIndex {
    frames: DicomEncapsulatedFrames,
    mapping: crate::DicomIndexMapping,
}

#[cfg(test)]
pub(super) fn scan_encapsulated_frames_raw_little_endian(
    path: &Path,
    number_of_frames: u32,
) -> Result<Option<DicomEncapsulatedFrames>, WsiError> {
    scan_encapsulated_frames_raw_little_endian_controlled(path, number_of_frames, None)
        .map(|index| index.map(|index| index.frames))
}

fn scan_encapsulated_frames_raw_little_endian_controlled(
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
    let index = build_encapsulated_frame_index_with_mapping(
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
    validate_extended_offset_table_shape(path, tables, number_of_frames)?;
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

pub(super) fn checked_padded_fragment_len(
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
    file.seek(SeekFrom::Start(0))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let mut preamble = [0u8; 132];
    let has_magic = file
        .read_exact(&mut preamble)
        .is_ok_and(|()| &preamble[128..] == b"DICM");
    if has_magic {
        file.seek(SeekFrom::Start(128))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
        if FileMetaTable::from_reader(&mut *file).is_ok() {
            return Ok(());
        }
        file.seek(SeekFrom::Start(132))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
    } else {
        file.seek(SeekFrom::Start(0))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
    }
    Ok(())
}

pub(super) fn read_extended_offset_tables_le(
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
    const MAX_EXTENDED_OFFSET_TABLE_BYTES: u32 = 256 * 1024 * 1024;
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

pub(super) fn read_extended_offset_tables_with_reader<R: Read + Seek>(
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
    const TABLE_READ_CHUNK_BYTES: usize = 64 * 1024;
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

#[cfg(test)]
pub(super) fn scan_raw_encapsulated_pixel_sequence_with_reader<R: Read + Seek>(
    reader: &mut R,
    path: &Path,
    pixel_data_offset: u64,
    file_len: u64,
    number_of_frames: Option<u32>,
) -> Result<(Vec<DicomFragmentRef>, Vec<u32>), WsiError> {
    scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
        reader,
        path,
        pixel_data_offset,
        file_len,
        number_of_frames,
        None,
    )
}

pub(super) fn scan_raw_encapsulated_pixel_sequence_with_reader_controlled<R: Read + Seek>(
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

pub(super) fn read_basic_offset_table_at(
    file: &mut (impl Read + Seek),
    path: &Path,
    offset: u64,
    len: u32,
    number_of_frames: Option<u32>,
    control: Option<&crate::ReadControl>,
) -> Result<Vec<u32>, WsiError> {
    const TABLE_READ_CHUNK_BYTES: usize = 64 * 1024;
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

pub(super) fn validate_basic_offset_table_len(
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

pub(super) fn read_exact_at(
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

pub(super) fn build_encapsulated_frame_index(
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

fn build_encapsulated_frame_index_with_mapping(
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

pub(super) fn frame_ranges_from_extended_offsets(
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

fn validate_extended_offset_table_shape(
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
