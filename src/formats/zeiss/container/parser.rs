use std::io::{Read, Seek};

use super::{directory, format::*, model::*, payload, ParsedCziContainer};
use crate::error::WsiError;
use crate::formats::zeiss::preflight::{
    checked_add, preflight_error as invalid, read_exact_at, read_i32, read_segment_header, read_u64,
};
use crate::formats::zeiss::MAX_CZI_ATTACHMENTS;

pub(super) fn parse_file(
    reader: &mut (impl Read + Seek),
    file_len: u64,
) -> Result<ParsedCziContainer, WsiError> {
    let header = parse_file_header(reader, file_len)?;
    let (subblocks, statistics) =
        directory::parse_directory(reader, file_len, header.subblock_directory_position)?;
    let attachments = if header.attachment_directory_position == 0 {
        Vec::new()
    } else {
        parse_attachment_directory(reader, file_len, header.attachment_directory_position)?
    };
    Ok(ParsedCziContainer {
        header,
        subblocks,
        attachments,
        statistics,
    })
}

fn parse_file_header(
    reader: &mut (impl Read + Seek),
    file_len: u64,
) -> Result<FileHeaderInfo, WsiError> {
    read_segment_header(
        reader,
        file_len,
        0,
        FILE_MAGIC,
        FILE_HEADER_DATA_BYTES,
        "file header",
    )?;
    let bytes = read_exact_at(
        reader,
        file_len,
        SEGMENT_HEADER_BYTES,
        FILE_HEADER_DATA_BYTES as usize,
        "file header",
    )?;
    Ok(FileHeaderInfo {
        major: read_i32(&bytes, 0, "major version")?,
        minor: read_i32(&bytes, 4, "minor version")?,
        primary_file_guid: parse_guid(&bytes[16..32])?,
        file_guid: parse_guid(&bytes[32..48])?,
        file_part: read_i32(&bytes, 48, "file part")?,
        subblock_directory_position: read_u64(&bytes, 52, "subblock directory offset")?,
        metadata_position: read_u64(&bytes, 60, "metadata offset")?,
        update_pending: read_i32(&bytes, 68, "update-pending flag")? != 0,
        attachment_directory_position: read_u64(&bytes, 72, "attachment directory offset")?,
    })
}

fn parse_attachment_directory(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
) -> Result<Vec<AttachmentInfo>, WsiError> {
    let effective_size = read_segment_header(
        reader,
        file_len,
        offset,
        ATTACHMENT_DIRECTORY_MAGIC,
        ATTACHMENT_DIRECTORY_FIXED_BYTES,
        "attachment directory",
    )?;
    let fixed_offset = checked_add(offset, SEGMENT_HEADER_BYTES, "attachment directory")?;
    let fixed = read_exact_at(
        reader,
        file_len,
        fixed_offset,
        ATTACHMENT_DIRECTORY_FIXED_BYTES as usize,
        "attachment directory header",
    )?;
    let count = read_i32(&fixed, 0, "attachment count")?;
    if count <= 0 {
        return Ok(Vec::new());
    }
    let count = usize::try_from(count).map_err(|_| invalid("invalid CZI attachment count"))?;
    if count > MAX_CZI_ATTACHMENTS {
        return Err(invalid("CZI attachment count exceeds its safety limit"));
    }
    let payload_bytes = effective_size - ATTACHMENT_DIRECTORY_FIXED_BYTES;
    let required = u64::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(128))
        .ok_or_else(|| invalid("CZI attachment directory size overflows"))?;
    if required > payload_bytes {
        return Err(invalid("CZI attachment directory is truncated"));
    }
    let mut cursor = checked_add(
        fixed_offset,
        ATTACHMENT_DIRECTORY_FIXED_BYTES,
        "attachment directory entries",
    )?;
    let end = checked_add(cursor, payload_bytes, "attachment directory payload")?;
    let mut attachments = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = read_within(
            reader,
            file_len,
            &mut cursor,
            end,
            128,
            "attachment directory entry",
        )?;
        if entry.get(..2) != Some(b"A1".as_slice()) {
            continue;
        }
        let file_position = read_u64(&entry, 12, "attachment file offset")?;
        let data_size = payload::read_attachment_data_size(reader, file_len, file_position)?;
        attachments.push(AttachmentInfo {
            index: attachments.len(),
            file_position,
            file_part: read_i32(&entry, 20, "attachment file part")?,
            content_guid: parse_guid(&entry[24..40])?,
            content_file_type: parse_fixed_string(&entry[40..48]),
            name: parse_fixed_string(&entry[48..128]),
            data_size,
        });
    }
    Ok(attachments)
}
