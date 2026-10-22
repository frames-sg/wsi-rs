use std::io::{Read, Seek};

use crate::error::WsiError;
use crate::formats::zeiss::preflight::{checked_add, preflight_error as invalid, read_exact_at};

pub(super) const SEGMENT_HEADER_BYTES: u64 = 32;
pub(super) const FILE_HEADER_DATA_BYTES: u64 = 512;
pub(super) const DIRECTORY_FIXED_BYTES: u64 = 128;
pub(super) const METADATA_FIXED_BYTES: u64 = 256;
pub(super) const ATTACHMENT_FIXED_BYTES: u64 = 256;
pub(super) const ATTACHMENT_DIRECTORY_FIXED_BYTES: u64 = 256;
pub(super) const DIRECTORY_ENTRY_FIXED_BYTES: u64 = 32;
pub(super) const DIMENSION_ENTRY_BYTES: u64 = 20;
pub(super) const SUBBLOCK_FIXED_BYTES: u64 = 256;
pub(super) const SUBBLOCK_ENTRY_PREFIX_BYTES: u64 = 16;
pub(super) const MAX_SUBBLOCK_SIDE_DATA_BYTES: u64 = 16 * 1024 * 1024;

pub(super) const FILE_MAGIC: &[u8; 16] = b"ZISRAWFILE\0\0\0\0\0\0";
pub(super) const DIRECTORY_MAGIC: &[u8; 16] = b"ZISRAWDIRECTORY\0";
pub(super) const METADATA_MAGIC: &[u8; 16] = b"ZISRAWMETADATA\0\0";
pub(super) const ATTACHMENT_DIRECTORY_MAGIC: &[u8; 16] = b"ZISRAWATTDIR\0\0\0\0";
pub(super) const SUBBLOCK_MAGIC: &[u8; 16] = b"ZISRAWSUBBLOCK\0\0";
pub(super) const ATTACHMENT_MAGIC: &[u8; 16] = b"ZISRAWATTACH\0\0\0\0";

pub(super) fn read_within(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    cursor: &mut u64,
    end: u64,
    bytes: u64,
    label: &str,
) -> Result<Vec<u8>, WsiError> {
    let next = checked_add(*cursor, bytes, label)?;
    if next > end {
        return Err(invalid(format!("CZI {label} exceeds its segment")));
    }
    let value = read_exact_at(reader, file_len, *cursor, to_usize(bytes, label)?, label)?;
    *cursor = next;
    Ok(value)
}
pub(super) fn parse_guid(bytes: &[u8]) -> Result<String, WsiError> {
    let bytes: &[u8; 16] = bytes
        .try_into()
        .map_err(|_| invalid("CZI GUID must contain 16 bytes"))?;
    let first = u32::from_le_bytes(bytes[0..4].try_into().expect("fixed GUID field"));
    let second = u16::from_le_bytes(bytes[4..6].try_into().expect("fixed GUID field"));
    let third = u16::from_le_bytes(bytes[6..8].try_into().expect("fixed GUID field"));
    Ok(format!(
        "{first:08x}-{second:04x}-{third:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    ))
}
pub(super) fn parse_fixed_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}
pub(super) fn to_usize(value: u64, label: &str) -> Result<usize, WsiError> {
    usize::try_from(value).map_err(|_| invalid(format!("CZI {label} exceeds address space")))
}
