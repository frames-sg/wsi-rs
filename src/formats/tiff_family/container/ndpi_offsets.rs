use std::path::Path;

use super::super::error::TiffParseError;
use super::model::tags;
use super::parse::ParseReader;

// ── NDPI offset fixup ──────────────────────────────────────────────

/// NDPI stores >4GB offsets using only the low 32 bits in classic TIFF fields.
/// Heuristic: reconstruct high bits from the IFD's own offset.
/// Ported from the established tifflike offset-reconstruction behavior.
pub(super) fn fix_offset_ndpi(diroff: u64, offset: u64) -> u64 {
    let mut result = (diroff & !u64::from(u32::MAX)) | (offset & u64::from(u32::MAX));
    if result >= diroff {
        result = result.saturating_sub(u64::from(u32::MAX) + 1).min(result);
    }
    result
}

pub(super) fn is_ndpi_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(ext) if ext.eq_ignore_ascii_case("ndpi")
    )
}

pub(super) fn is_ndpi_data_offset_tag(tag: u16) -> bool {
    matches!(tag, tags::STRIP_OFFSETS | tags::TILE_OFFSETS)
}

pub(super) fn repair_ndpi_first_ifd_offset(
    reader: &mut ParseReader,
    raw_offset: u64,
    file_len: u64,
) -> Result<u64, TiffParseError> {
    let four_gb = u64::from(u32::MAX) + 1;
    let mut candidate = raw_offset;
    while candidate < file_len {
        if is_plausible_ifd_offset(reader, candidate, file_len)? {
            return Ok(candidate);
        }
        candidate = match candidate.checked_add(four_gb) {
            Some(next) => next,
            None => break,
        };
    }
    if raw_offset >= file_len {
        return Err(TiffParseError::Structure(format!(
            "NDPI first IFD offset {raw_offset} is outside file length {file_len}; file may be truncated"
        )));
    }
    Ok(raw_offset)
}

pub(super) fn is_plausible_ifd_offset(
    reader: &mut ParseReader,
    offset: u64,
    file_len: u64,
) -> Result<bool, TiffParseError> {
    if offset + 2 > file_len {
        return Ok(false);
    }
    reader.seek(offset)?;
    let entry_count = reader.read_u16()? as u64;
    if entry_count == 0 || entry_count > 4096 {
        return Ok(false);
    }
    let ifd_bytes = 2u64
        .checked_add(entry_count.saturating_mul(12))
        .and_then(|value| value.checked_add(8))
        .unwrap_or(u64::MAX);
    Ok(offset.saturating_add(ifd_bytes) <= file_len)
}
