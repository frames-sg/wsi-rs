use std::io::{Read, Seek};

use super::{format::*, model::*};
use crate::error::WsiError;
use crate::formats::zeiss::preflight::{
    checked_add, preflight_error as invalid, read_exact_at, read_segment_header, read_u32, read_u64,
};
use crate::formats::zeiss::MAX_CZI_METADATA_BYTES;

pub(super) fn read_metadata_xml(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
) -> Result<String, WsiError> {
    if offset == 0 {
        return Ok(String::new());
    }
    let effective_size = read_segment_header(
        reader,
        file_len,
        offset,
        METADATA_MAGIC,
        METADATA_FIXED_BYTES,
        "metadata",
    )?;
    let fixed_offset = checked_add(offset, SEGMENT_HEADER_BYTES, "metadata header")?;
    let fixed = read_exact_at(
        reader,
        file_len,
        fixed_offset,
        METADATA_FIXED_BYTES as usize,
        "metadata header",
    )?;
    let xml_bytes = u64::from(read_u32(&fixed, 0, "metadata length")?);
    if xml_bytes > MAX_CZI_METADATA_BYTES {
        return Err(invalid(format!(
            "CZI metadata declares {xml_bytes} bytes, exceeding the {MAX_CZI_METADATA_BYTES}-byte safety limit"
        )));
    }
    if xml_bytes > effective_size - METADATA_FIXED_BYTES {
        return Err(invalid("CZI metadata exceeds its segment"));
    }
    let xml_offset = checked_add(fixed_offset, METADATA_FIXED_BYTES, "metadata payload")?;
    let xml = read_exact_at(
        reader,
        file_len,
        xml_offset,
        to_usize(xml_bytes, "metadata length")?,
        "metadata payload",
    )?;
    String::from_utf8(xml).map_err(|error| invalid(format!("CZI metadata is not UTF-8: {error}")))
}

pub(super) fn read_attachment_blob(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    info: &AttachmentInfo,
) -> Result<AttachmentBlob, WsiError> {
    let effective_size = read_segment_header(
        reader,
        file_len,
        info.file_position,
        ATTACHMENT_MAGIC,
        ATTACHMENT_FIXED_BYTES,
        "attachment",
    )?;
    let fixed_offset = checked_add(
        info.file_position,
        SEGMENT_HEADER_BYTES,
        "attachment header",
    )?;
    let fixed = read_exact_at(
        reader,
        file_len,
        fixed_offset,
        ATTACHMENT_FIXED_BYTES as usize,
        "attachment header",
    )?;
    let data_size = read_u64(&fixed, 0, "attachment data length")?;
    if data_size > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
        return Err(invalid("CZI attachment exceeds the encoded input limit"));
    }
    if data_size != info.data_size {
        return Err(invalid(format!(
            "CZI attachment {} size changed from {} to {data_size}",
            info.name, info.data_size
        )));
    }
    if data_size > effective_size - ATTACHMENT_FIXED_BYTES {
        return Err(invalid("CZI attachment data exceeds its segment"));
    }
    let data_offset = checked_add(fixed_offset, ATTACHMENT_FIXED_BYTES, "attachment data")?;
    let data = read_exact_at(
        reader,
        file_len,
        data_offset,
        to_usize(data_size, "attachment data length")?,
        "attachment data",
    )?;
    Ok(AttachmentBlob {
        info: info.clone(),
        data,
    })
}

/// Stored data bytes of one subblock, read from its fixed header alone.
pub(super) fn read_subblock_data_size(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    info: &DirectorySubBlockInfo,
) -> Result<u64, WsiError> {
    read_segment_header(
        reader,
        file_len,
        info.file_position,
        SUBBLOCK_MAGIC,
        SUBBLOCK_FIXED_BYTES,
        "subblock",
    )?;
    let fixed_offset = checked_add(info.file_position, SEGMENT_HEADER_BYTES, "subblock header")?;
    let fixed = read_exact_at(
        reader,
        file_len,
        fixed_offset,
        SUBBLOCK_FIXED_BYTES as usize,
        "subblock header",
    )?;
    let data_bytes = read_u64(&fixed, 8, "subblock data length")?;
    if data_bytes > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
        return Err(invalid("CZI subblock payload exceeds its safety limit"));
    }
    Ok(data_bytes)
}

pub(super) fn read_raw_subblock(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    info: &DirectorySubBlockInfo,
) -> Result<RawSubBlock, WsiError> {
    let effective_size = read_segment_header(
        reader,
        file_len,
        info.file_position,
        SUBBLOCK_MAGIC,
        SUBBLOCK_FIXED_BYTES,
        "subblock",
    )?;
    let fixed_offset = checked_add(info.file_position, SEGMENT_HEADER_BYTES, "subblock header")?;
    let fixed = read_exact_at(
        reader,
        file_len,
        fixed_offset,
        SUBBLOCK_FIXED_BYTES as usize,
        "subblock header",
    )?;
    let metadata_bytes = u64::from(read_u32(&fixed, 0, "subblock metadata length")?);
    let attachment_bytes = u64::from(read_u32(&fixed, 4, "subblock attachment length")?);
    let data_bytes = read_u64(&fixed, 8, "subblock data length")?;
    if metadata_bytes > MAX_SUBBLOCK_SIDE_DATA_BYTES
        || attachment_bytes > MAX_SUBBLOCK_SIDE_DATA_BYTES
        || data_bytes > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES
    {
        return Err(invalid("CZI subblock payload exceeds its safety limit"));
    }
    if fixed.get(16..18) != Some(b"DV".as_slice()) {
        return Err(invalid("unsupported CZI subblock schema"));
    }
    let dimension_count = u64::from(read_u32(&fixed, 44, "subblock dimension count")?);
    if dimension_count > 1_024 {
        return Err(invalid("invalid CZI subblock dimension count"));
    }
    let dynamic_header = SUBBLOCK_ENTRY_PREFIX_BYTES
        .checked_add(DIRECTORY_ENTRY_FIXED_BYTES)
        .and_then(|bytes| bytes.checked_add(dimension_count.checked_mul(DIMENSION_ENTRY_BYTES)?))
        .ok_or_else(|| invalid("CZI subblock header size overflows"))?;
    let header_bytes = dynamic_header.max(SUBBLOCK_FIXED_BYTES);
    let actual_bytes = header_bytes
        .checked_add(metadata_bytes)
        .and_then(|bytes| bytes.checked_add(data_bytes))
        .and_then(|bytes| bytes.checked_add(attachment_bytes))
        .ok_or_else(|| invalid("CZI subblock size overflows"))?;
    if actual_bytes > effective_size {
        return Err(invalid("CZI subblock payload exceeds its segment"));
    }
    let metadata_offset = checked_add(fixed_offset, header_bytes, "subblock metadata")?;
    let data_offset = checked_add(metadata_offset, metadata_bytes, "subblock data")?;
    let attachment_offset = checked_add(data_offset, data_bytes, "subblock attachment")?;
    Ok(RawSubBlock {
        info: info.clone(),
        metadata: read_exact_at(
            reader,
            file_len,
            metadata_offset,
            to_usize(metadata_bytes, "subblock metadata length")?,
            "subblock metadata",
        )?,
        data: read_exact_at(
            reader,
            file_len,
            data_offset,
            to_usize(data_bytes, "subblock data length")?,
            "subblock data",
        )?,
        attachment: read_exact_at(
            reader,
            file_len,
            attachment_offset,
            to_usize(attachment_bytes, "subblock attachment length")?,
            "subblock attachment",
        )?,
    })
}

pub(super) fn read_attachment_data_size(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
) -> Result<u64, WsiError> {
    read_segment_header(
        reader,
        file_len,
        offset,
        ATTACHMENT_MAGIC,
        ATTACHMENT_FIXED_BYTES,
        "attachment",
    )?;
    let prefix = read_exact_at(
        reader,
        file_len,
        checked_add(offset, SEGMENT_HEADER_BYTES, "attachment header")?,
        8,
        "attachment header",
    )?;
    read_u64(&prefix, 0, "attachment data length")
}
