use super::*;

/// Write a u16 in the given endianness.
pub(super) fn write_u16(buf: &mut Vec<u8>, endian: Endian, val: u16) {
    match endian {
        Endian::Little => buf.extend_from_slice(&val.to_le_bytes()),
        Endian::Big => buf.extend_from_slice(&val.to_be_bytes()),
    }
}

/// Write a u32 in the given endianness.
pub(super) fn write_u32(buf: &mut Vec<u8>, endian: Endian, val: u32) {
    match endian {
        Endian::Little => buf.extend_from_slice(&val.to_le_bytes()),
        Endian::Big => buf.extend_from_slice(&val.to_be_bytes()),
    }
}

/// Write a u64 in the given endianness.
pub(super) fn write_u64(buf: &mut Vec<u8>, endian: Endian, val: u64) {
    match endian {
        Endian::Little => buf.extend_from_slice(&val.to_le_bytes()),
        Endian::Big => buf.extend_from_slice(&val.to_be_bytes()),
    }
}

/// A synthetic IFD entry for test data construction.
pub(super) struct SyntheticEntry {
    pub(super) tag: u16,
    pub(super) tiff_type: u16,
    pub(super) count: u64,
    /// Inline data (used when total_bytes <= slot_size).
    /// If None and data is out-of-line, `out_of_line_data` is used.
    pub(super) inline_data: Option<Vec<u8>>,
    /// Out-of-line data to be written at a computed offset.
    pub(super) out_of_line_data: Option<Vec<u8>>,
}

/// Build a minimal classic TIFF with one IFD.
/// Returns (bytes, ifd_offset).
pub(super) fn make_classic_tiff_single(endian: Endian, entries: &[SyntheticEntry]) -> Vec<u8> {
    let mut buf = Vec::new();
    // Byte order
    match endian {
        Endian::Little => buf.extend_from_slice(b"II"),
        Endian::Big => buf.extend_from_slice(b"MM"),
    }
    // Magic
    write_u16(&mut buf, endian, 42);
    // First IFD offset (immediately after the header, at offset 8)
    let ifd_offset = 8u32;
    write_u32(&mut buf, endian, ifd_offset);

    // IFD entry count
    let entry_count = entries.len() as u16;
    write_u16(&mut buf, endian, entry_count);

    // Compute where out-of-line data goes:
    // After header(8) + entry_count(2) + entries(12 each) + next_ifd_offset(4)
    let mut ool_offset = 8u64 + 2 + (entries.len() as u64 * 12) + 4;

    // Collect out-of-line data with their offsets
    let mut ool_chunks: Vec<(u64, Vec<u8>)> = Vec::new();

    for entry in entries {
        write_u16(&mut buf, endian, entry.tag);
        write_u16(&mut buf, endian, entry.tiff_type);
        write_u32(&mut buf, endian, entry.count as u32);

        let type_size = TiffType::from_u16(entry.tiff_type)
            .map(|t| t.byte_size())
            .unwrap_or(1);
        let total_bytes = entry.count * type_size;

        if total_bytes <= 4 {
            // Inline: write up to 4 bytes, pad with zeros
            let data = entry.inline_data.as_deref().unwrap_or(&[]);
            let mut slot = [0u8; 4];
            let copy_len = data.len().min(4);
            slot[..copy_len].copy_from_slice(&data[..copy_len]);
            buf.extend_from_slice(&slot);
        } else {
            // Out-of-line: write offset
            let data = entry
                .out_of_line_data
                .as_ref()
                .expect("out-of-line entry must have out_of_line_data");
            write_u32(&mut buf, endian, ool_offset as u32);
            ool_chunks.push((ool_offset, data.clone()));
            ool_offset += data.len() as u64;
        }
    }

    // Next IFD offset = 0 (no more IFDs)
    write_u32(&mut buf, endian, 0);

    // Write out-of-line data
    for (_offset, data) in &ool_chunks {
        buf.extend_from_slice(data);
    }

    buf
}

/// Build a minimal BigTIFF with one IFD.
pub(super) fn make_bigtiff_single(endian: Endian, entries: &[SyntheticEntry]) -> Vec<u8> {
    let mut buf = Vec::new();
    // Byte order
    match endian {
        Endian::Little => buf.extend_from_slice(b"II"),
        Endian::Big => buf.extend_from_slice(b"MM"),
    }
    // Magic 43
    write_u16(&mut buf, endian, 43);
    // Offset size = 8
    write_u16(&mut buf, endian, 8);
    // Reserved = 0
    write_u16(&mut buf, endian, 0);
    // First IFD offset (immediately after header, at offset 16)
    let ifd_offset = 16u64;
    write_u64(&mut buf, endian, ifd_offset);

    // IFD entry count (8 bytes for BigTIFF)
    let entry_count = entries.len() as u64;
    write_u64(&mut buf, endian, entry_count);

    // Compute where out-of-line data goes:
    // After header(16) + entry_count(8) + entries(20 each) + next_ifd_offset(8)
    let mut ool_offset = 16u64 + 8 + (entries.len() as u64 * 20) + 8;

    let mut ool_chunks: Vec<(u64, Vec<u8>)> = Vec::new();

    for entry in entries {
        write_u16(&mut buf, endian, entry.tag);
        write_u16(&mut buf, endian, entry.tiff_type);
        write_u64(&mut buf, endian, entry.count);

        let type_size = TiffType::from_u16(entry.tiff_type)
            .map(|t| t.byte_size())
            .unwrap_or(1);
        let total_bytes = entry.count * type_size;

        if total_bytes <= 8 {
            // Inline: write up to 8 bytes, pad with zeros
            let data = entry.inline_data.as_deref().unwrap_or(&[]);
            let mut slot = [0u8; 8];
            let copy_len = data.len().min(8);
            slot[..copy_len].copy_from_slice(&data[..copy_len]);
            buf.extend_from_slice(&slot);
        } else {
            // Out-of-line: write offset
            let data = entry
                .out_of_line_data
                .as_ref()
                .expect("out-of-line entry must have out_of_line_data");
            write_u64(&mut buf, endian, ool_offset);
            ool_chunks.push((ool_offset, data.clone()));
            ool_offset += data.len() as u64;
        }
    }

    // Next IFD offset = 0
    write_u64(&mut buf, endian, 0);

    // Write out-of-line data
    for (_offset, data) in &ool_chunks {
        buf.extend_from_slice(data);
    }

    buf
}

/// Write synthetic TIFF bytes to a tempfile, return the path.
pub(super) fn write_tiff_tempfile(data: &[u8]) -> tempfile::NamedTempFile {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    tmp.write_all(data).unwrap();
    tmp.flush().unwrap();
    tmp
}

// ── Header parsing tests ──────────────────────────────────

/// Build a classic TIFF with two chained IFDs, each with the given entries.
pub(super) fn make_classic_tiff_two_ifds(
    endian: Endian,
    entries1: &[SyntheticEntry],
    entries2: &[SyntheticEntry],
) -> Vec<u8> {
    let mut buf = Vec::new();
    // Byte order
    match endian {
        Endian::Little => buf.extend_from_slice(b"II"),
        Endian::Big => buf.extend_from_slice(b"MM"),
    }
    // Magic
    write_u16(&mut buf, endian, 42);
    // First IFD offset = 8
    write_u32(&mut buf, endian, 8);

    // === IFD 1 ===
    let ifd1_offset = 8u64;
    write_u16(&mut buf, endian, entries1.len() as u16);

    // Compute IFD1 total size: 2 + entries*12 + 4
    let ifd1_size = 2 + (entries1.len() as u64 * 12) + 4;
    // IFD2 starts after IFD1 + any OOL data from IFD1
    // For simplicity, assume no OOL data in entries1
    let ifd2_offset = ifd1_offset + ifd1_size;

    for entry in entries1 {
        write_u16(&mut buf, endian, entry.tag);
        write_u16(&mut buf, endian, entry.tiff_type);
        write_u32(&mut buf, endian, entry.count as u32);
        let data = entry.inline_data.as_deref().unwrap_or(&[0, 0, 0, 0]);
        let mut slot = [0u8; 4];
        let copy_len = data.len().min(4);
        slot[..copy_len].copy_from_slice(&data[..copy_len]);
        buf.extend_from_slice(&slot);
    }

    // Next IFD offset -> IFD2
    write_u32(&mut buf, endian, ifd2_offset as u32);

    // === IFD 2 ===
    write_u16(&mut buf, endian, entries2.len() as u16);

    for entry in entries2 {
        write_u16(&mut buf, endian, entry.tag);
        write_u16(&mut buf, endian, entry.tiff_type);
        write_u32(&mut buf, endian, entry.count as u32);
        let data = entry.inline_data.as_deref().unwrap_or(&[0, 0, 0, 0]);
        let mut slot = [0u8; 4];
        let copy_len = data.len().min(4);
        slot[..copy_len].copy_from_slice(&data[..copy_len]);
        buf.extend_from_slice(&slot);
    }

    // Next IFD offset = 0 (end)
    write_u32(&mut buf, endian, 0);

    buf
}

// ── IFD chain walking tests ───────────────────────────────

/// Build a classic TIFF with one main IFD that has a SUB_IFDS tag
/// pointing to one SubIFD.
pub(super) fn make_classic_tiff_with_subifd(endian: Endian) -> Vec<u8> {
    let mut buf = Vec::new();
    match endian {
        Endian::Little => buf.extend_from_slice(b"II"),
        Endian::Big => buf.extend_from_slice(b"MM"),
    }
    write_u16(&mut buf, endian, 42);
    write_u32(&mut buf, endian, 8); // first IFD at 8

    // Main IFD: 2 entries (IMAGE_WIDTH + SUB_IFDS)
    write_u16(&mut buf, endian, 2);

    // Entry 1: IMAGE_WIDTH = 1024
    write_u16(&mut buf, endian, tags::IMAGE_WIDTH);
    write_u16(&mut buf, endian, 4); // LONG
    write_u32(&mut buf, endian, 1);
    write_u32(&mut buf, endian, 1024);

    // Entry 2: SUB_IFDS — inline, count=1, pointing to SubIFD
    // Main IFD: header(8) + count(2) + 2*entries(24) + next(4) = 38
    // SubIFD will be at offset 38
    let sub_ifd_offset = 38u32;
    write_u16(&mut buf, endian, tags::SUB_IFDS);
    write_u16(&mut buf, endian, 4); // LONG (IFD offsets as LONG)
    write_u32(&mut buf, endian, 1);
    write_u32(&mut buf, endian, sub_ifd_offset);

    // Next IFD offset = 0
    write_u32(&mut buf, endian, 0);

    // === SubIFD at offset 38 ===
    assert_eq!(buf.len(), sub_ifd_offset as usize);
    write_u16(&mut buf, endian, 1); // 1 entry

    // Entry: IMAGE_LENGTH = 768
    write_u16(&mut buf, endian, tags::IMAGE_LENGTH);
    write_u16(&mut buf, endian, 4); // LONG
    write_u32(&mut buf, endian, 1);
    write_u32(&mut buf, endian, 768);

    // Next IFD offset = 0
    write_u32(&mut buf, endian, 0);

    buf
}

// ── SubIFD tests ──────────────────────────────────────────
