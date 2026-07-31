use super::*;
use std::io::{Read, Seek, SeekFrom};

const SEGMENT_HEADER_BYTES: u64 = 32;
const FILE_HEADER_DATA_BYTES: u64 = 512;
const DIRECTORY_FIXED_BYTES: u64 = 128;
const METADATA_FIXED_BYTES: u64 = 256;
const ATTACHMENT_DIRECTORY_FIXED_BYTES: u64 = 256;
const ATTACHMENT_DIRECTORY_ENTRY_BYTES: u64 = 128;
const MIN_DIRECTORY_ENTRY_BYTES: u64 = 32;
const SUBBLOCK_FIXED_BYTES: u64 = 256;
const SUBBLOCK_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const SUBBLOCK_ATTACHMENT_BYTES: u64 = 16 * 1024 * 1024;
const DIRECTORY_MAGIC: &[u8; 16] = b"ZISRAWDIRECTORY\0";
const METADATA_MAGIC: &[u8; 16] = b"ZISRAWMETADATA\0\0";
const ATTACHMENT_DIRECTORY_MAGIC: &[u8; 16] = b"ZISRAWATTDIR\0\0\0\0";
const SUBBLOCK_MAGIC: &[u8; 16] = b"ZISRAWSUBBLOCK\0\0";

pub(super) fn preflight_czi_file(path: &Path) -> Result<(), WsiError> {
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let file_len = file.metadata()?.len();
    preflight_czi_reader(&mut file, file_len).map_err(|error| WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

pub(super) fn preflight_czi_subblock(path: &Path, offset: u64) -> Result<(), WsiError> {
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let file_len = file.metadata()?.len();
    preflight_czi_subblock_reader(&mut file, file_len, offset).map_err(|error| {
        WsiError::InvalidSlide {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })
}

fn preflight_czi_reader(reader: &mut (impl Read + Seek), file_len: u64) -> Result<(), WsiError> {
    let file_header_size = read_segment_header(
        reader,
        file_len,
        0,
        FILE_MAGIC,
        FILE_HEADER_DATA_BYTES,
        "file header",
    )?;
    if file_header_size < FILE_HEADER_DATA_BYTES {
        return Err(preflight_error("CZI file header is too short"));
    }
    let file_header = read_exact_at(
        reader,
        file_len,
        SEGMENT_HEADER_BYTES,
        FILE_HEADER_DATA_BYTES as usize,
        "file header",
    )?;
    let directory_offset = read_u64(&file_header, 52, "subblock directory offset")?;
    let metadata_offset = read_u64(&file_header, 60, "metadata offset")?;
    let attachment_directory_offset = read_u64(&file_header, 72, "attachment directory offset")?;

    preflight_directory(reader, file_len, directory_offset)?;
    if metadata_offset != 0 {
        preflight_metadata(reader, file_len, metadata_offset)?;
    }
    if attachment_directory_offset != 0 {
        preflight_attachment_directory(reader, file_len, attachment_directory_offset)?;
    }
    Ok(())
}

fn preflight_directory(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
) -> Result<(), WsiError> {
    let effective_size = read_segment_header(
        reader,
        file_len,
        offset,
        DIRECTORY_MAGIC,
        DIRECTORY_FIXED_BYTES,
        "subblock directory",
    )?;
    let payload_bytes = effective_size - DIRECTORY_FIXED_BYTES;
    if payload_bytes > MAX_CZI_DIRECTORY_BYTES {
        return Err(preflight_error(format!(
            "CZI subblock directory declares {payload_bytes} bytes, exceeding the {MAX_CZI_DIRECTORY_BYTES}-byte safety limit"
        )));
    }
    let fixed = read_exact_at(
        reader,
        file_len,
        checked_add(offset, SEGMENT_HEADER_BYTES, "directory header")?,
        DIRECTORY_FIXED_BYTES as usize,
        "subblock directory header",
    )?;
    let entry_count = read_i32(&fixed, 0, "subblock count")?;
    if entry_count < 0 {
        return Err(preflight_error("CZI subblock count is negative"));
    }
    let entry_count = entry_count as usize;
    if entry_count > MAX_CZI_SUBBLOCKS {
        return Err(preflight_error(format!(
            "CZI subblock count {entry_count} exceeds the {MAX_CZI_SUBBLOCKS}-entry safety limit"
        )));
    }
    let minimum_payload = u64::try_from(entry_count)
        .ok()
        .and_then(|count| count.checked_mul(MIN_DIRECTORY_ENTRY_BYTES))
        .ok_or_else(|| preflight_error("CZI subblock directory size overflows"))?;
    if minimum_payload > payload_bytes {
        return Err(preflight_error(format!(
            "CZI subblock directory is too short for {entry_count} entries"
        )));
    }
    Ok(())
}

fn preflight_metadata(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
) -> Result<(), WsiError> {
    let effective_size = read_segment_header(
        reader,
        file_len,
        offset,
        METADATA_MAGIC,
        METADATA_FIXED_BYTES,
        "metadata",
    )?;
    let fixed = read_exact_at(
        reader,
        file_len,
        checked_add(offset, SEGMENT_HEADER_BYTES, "metadata header")?,
        METADATA_FIXED_BYTES as usize,
        "metadata header",
    )?;
    let xml_bytes = u64::from(read_u32(&fixed, 0, "metadata length")?);
    if xml_bytes > MAX_CZI_METADATA_BYTES {
        return Err(preflight_error(format!(
            "CZI metadata declares {xml_bytes} bytes, exceeding the {MAX_CZI_METADATA_BYTES}-byte safety limit"
        )));
    }
    if xml_bytes > effective_size - METADATA_FIXED_BYTES {
        return Err(preflight_error(
            "CZI metadata payload exceeds its declared segment size",
        ));
    }
    Ok(())
}

fn preflight_attachment_directory(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
) -> Result<(), WsiError> {
    let effective_size = read_segment_header(
        reader,
        file_len,
        offset,
        ATTACHMENT_DIRECTORY_MAGIC,
        ATTACHMENT_DIRECTORY_FIXED_BYTES,
        "attachment directory",
    )?;
    let fixed = read_exact_at(
        reader,
        file_len,
        checked_add(offset, SEGMENT_HEADER_BYTES, "attachment directory header")?,
        ATTACHMENT_DIRECTORY_FIXED_BYTES as usize,
        "attachment directory header",
    )?;
    let entry_count = read_i32(&fixed, 0, "attachment count")?;
    if entry_count <= 0 {
        return Ok(());
    }
    let entry_count = entry_count as usize;
    if entry_count > MAX_CZI_ATTACHMENTS {
        return Err(preflight_error(format!(
            "CZI attachment count {entry_count} exceeds the {MAX_CZI_ATTACHMENTS}-entry safety limit"
        )));
    }
    let entries_bytes = u64::try_from(entry_count)
        .ok()
        .and_then(|count| count.checked_mul(ATTACHMENT_DIRECTORY_ENTRY_BYTES))
        .ok_or_else(|| preflight_error("CZI attachment directory size overflows"))?;
    if entries_bytes > effective_size - ATTACHMENT_DIRECTORY_FIXED_BYTES {
        return Err(preflight_error(
            "CZI attachment directory payload exceeds its declared segment size",
        ));
    }
    Ok(())
}

fn preflight_czi_subblock_reader(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
) -> Result<(), WsiError> {
    let effective_size = read_segment_header(
        reader,
        file_len,
        offset,
        SUBBLOCK_MAGIC,
        SUBBLOCK_FIXED_BYTES,
        "subblock",
    )?;
    let fixed = read_exact_at(
        reader,
        file_len,
        checked_add(offset, SEGMENT_HEADER_BYTES, "subblock header")?,
        SUBBLOCK_FIXED_BYTES as usize,
        "subblock header",
    )?;
    let metadata_bytes = u64::from(read_u32(&fixed, 0, "subblock metadata length")?);
    let attachment_bytes = u64::from(read_u32(&fixed, 4, "subblock attachment length")?);
    let data_bytes = read_u64(&fixed, 8, "subblock data length")?;
    if metadata_bytes > SUBBLOCK_METADATA_BYTES {
        return Err(preflight_error(format!(
            "CZI subblock metadata declares {metadata_bytes} bytes, exceeding the {SUBBLOCK_METADATA_BYTES}-byte safety limit"
        )));
    }
    if attachment_bytes > SUBBLOCK_ATTACHMENT_BYTES {
        return Err(preflight_error(format!(
            "CZI subblock attachment declares {attachment_bytes} bytes, exceeding the {SUBBLOCK_ATTACHMENT_BYTES}-byte safety limit"
        )));
    }
    if data_bytes > crate::core::limits::MAX_COMPRESSED_INPUT_BYTES {
        return Err(preflight_error(format!(
            "CZI subblock data declares {data_bytes} bytes, exceeding the {}-byte safety limit",
            crate::core::limits::MAX_COMPRESSED_INPUT_BYTES
        )));
    }
    if fixed.get(16..18) != Some(b"DV".as_slice()) {
        return Err(preflight_error("unsupported CZI subblock schema"));
    }
    let dimension_count = u64::from(read_u32(&fixed, 44, "subblock dimension count")?);
    if dimension_count > 1_024 {
        return Err(preflight_error(format!(
            "CZI subblock dimension count {dimension_count} exceeds the 1024-dimension safety limit"
        )));
    }
    let dynamic_header_bytes = 16_u64
        .checked_add(32)
        .and_then(|value| value.checked_add(dimension_count.checked_mul(20)?))
        .ok_or_else(|| preflight_error("CZI subblock header size overflows"))?;
    let actual_bytes = dynamic_header_bytes
        .max(SUBBLOCK_FIXED_BYTES)
        .checked_add(metadata_bytes)
        .and_then(|value| value.checked_add(data_bytes))
        .and_then(|value| value.checked_add(attachment_bytes))
        .ok_or_else(|| preflight_error("CZI subblock total size overflows"))?;
    if actual_bytes > effective_size {
        return Err(preflight_error(format!(
            "CZI subblock requires {actual_bytes} bytes, exceeding its {effective_size}-byte segment"
        )));
    }
    Ok(())
}

fn read_segment_header(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
    expected_magic: &[u8; 16],
    minimum_size: u64,
    label: &str,
) -> Result<u64, WsiError> {
    let header = read_exact_at(
        reader,
        file_len,
        offset,
        SEGMENT_HEADER_BYTES as usize,
        label,
    )?;
    if &header[..16] != expected_magic {
        return Err(preflight_error(format!("invalid CZI {label} magic")));
    }
    let allocated_size = read_u64(&header, 16, "allocated segment size")?;
    let used_size = read_u64(&header, 24, "used segment size")?;
    let effective_size = if used_size == 0 {
        allocated_size
    } else {
        used_size
    };
    if effective_size < minimum_size {
        return Err(preflight_error(format!(
            "CZI {label} declares {effective_size} bytes, smaller than the {minimum_size}-byte header"
        )));
    }
    let end = offset
        .checked_add(SEGMENT_HEADER_BYTES)
        .and_then(|value| value.checked_add(effective_size))
        .ok_or_else(|| preflight_error(format!("CZI {label} range overflows")))?;
    if end > file_len {
        return Err(preflight_error(format!(
            "CZI {label} range ends at {end}, beyond file length {file_len}"
        )));
    }
    Ok(effective_size)
}

fn read_exact_at(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
    len: usize,
    label: &str,
) -> Result<Vec<u8>, WsiError> {
    let end = offset
        .checked_add(u64::try_from(len).unwrap_or(u64::MAX))
        .ok_or_else(|| preflight_error(format!("CZI {label} range overflows")))?;
    if end > file_len {
        return Err(preflight_error(format!(
            "CZI {label} is truncated at file offset {offset}"
        )));
    }
    reader.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn checked_add(left: u64, right: u64, label: &str) -> Result<u64, WsiError> {
    left.checked_add(right)
        .ok_or_else(|| preflight_error(format!("CZI {label} offset overflows")))
}

fn read_u32(bytes: &[u8], offset: usize, label: &str) -> Result<u32, WsiError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| preflight_error(format!("CZI {label} is truncated")))?;
    Ok(u32::from_le_bytes(value.try_into().map_err(|_| {
        preflight_error(format!("CZI {label} is malformed"))
    })?))
}

fn read_i32(bytes: &[u8], offset: usize, label: &str) -> Result<i32, WsiError> {
    read_u32(bytes, offset, label).map(|value| i32::from_le_bytes(value.to_le_bytes()))
}

fn read_u64(bytes: &[u8], offset: usize, label: &str) -> Result<u64, WsiError> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| preflight_error(format!("CZI {label} is truncated")))?;
    Ok(u64::from_le_bytes(value.try_into().map_err(|_| {
        preflight_error(format!("CZI {label} is malformed"))
    })?))
}

fn preflight_error(message: impl Into<String>) -> WsiError {
    WsiError::DisplayConversion(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const FILE_HEADER_BYTES: usize = 32 + 512;
    const DIRECTORY_OFFSET: usize = FILE_HEADER_BYTES;

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn base_czi(metadata_offset: u64) -> Vec<u8> {
        let mut bytes = vec![0; DIRECTORY_OFFSET + 32 + 128];
        bytes[..16].copy_from_slice(b"ZISRAWFILE\0\0\0\0\0\0");
        write_u64(&mut bytes, 16, 512);
        write_u64(&mut bytes, 24, 512);
        write_u64(&mut bytes, 32 + 52, DIRECTORY_OFFSET as u64);
        write_u64(&mut bytes, 32 + 60, metadata_offset);
        bytes[DIRECTORY_OFFSET..DIRECTORY_OFFSET + 16].copy_from_slice(b"ZISRAWDIRECTORY\0");
        write_u64(&mut bytes, DIRECTORY_OFFSET + 16, 128);
        write_u64(&mut bytes, DIRECTORY_OFFSET + 24, 128);
        bytes
    }

    #[test]
    fn czi_preflight_rejects_excessive_directory_entry_count() {
        let mut bytes = base_czi(0);
        write_u32(&mut bytes, DIRECTORY_OFFSET + 32, (1_000_000_u32) + 1);
        let mut cursor = Cursor::new(bytes.clone());
        let error = preflight_czi_reader(&mut cursor, bytes.len() as u64)
            .expect_err("excessive CZI directory must be rejected");
        assert!(error.to_string().contains("subblock count"), "{error}");
    }

    #[test]
    fn czi_preflight_rejects_oversized_metadata_declaration() {
        let metadata_offset = (DIRECTORY_OFFSET + 32 + 128) as u64;
        let mut bytes = base_czi(metadata_offset);
        bytes.resize(metadata_offset as usize + 32 + 256, 0);
        let offset = metadata_offset as usize;
        bytes[offset..offset + 16].copy_from_slice(b"ZISRAWMETADATA\0\0");
        write_u64(&mut bytes, offset + 16, 256);
        write_u64(&mut bytes, offset + 24, 256);
        write_u32(&mut bytes, offset + 32, (32 * 1024 * 1024) + 1);

        let mut cursor = Cursor::new(bytes.clone());
        let error = preflight_czi_reader(&mut cursor, bytes.len() as u64)
            .expect_err("oversized CZI metadata must be rejected");
        assert!(error.to_string().contains("metadata"), "{error}");
    }

    #[test]
    fn czi_subblock_preflight_rejects_oversized_payload_declaration() {
        let mut bytes = vec![0; 32 + 256];
        bytes[..16].copy_from_slice(b"ZISRAWSUBBLOCK\0\0");
        write_u64(&mut bytes, 16, 256);
        write_u64(&mut bytes, 24, 256);
        write_u64(
            &mut bytes,
            32 + 8,
            crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 1,
        );
        bytes[32 + 16..32 + 18].copy_from_slice(b"DV");

        let mut cursor = Cursor::new(bytes.clone());
        let error = preflight_czi_subblock_reader(&mut cursor, bytes.len() as u64, 0)
            .expect_err("oversized CZI subblock must be rejected");
        assert!(error.to_string().contains("subblock data"), "{error}");
    }
}
