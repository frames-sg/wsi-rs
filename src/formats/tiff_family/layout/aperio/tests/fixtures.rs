use std::io::Write;
use tempfile::NamedTempFile;

// ── Synthetic TIFF builder ───────────────────────────────────────

/// Represents one tag to write into a synthetic IFD.
/// For out-of-line data (ASCII strings, byte arrays), use `ool_data`.
pub(super) struct SyntheticTag {
    tag: u16,
    tiff_type: u16,
    count: u32,
    /// Inline value (up to 4 bytes). Ignored when `ool_data` is Some.
    inline_value: [u8; 4],
    /// Out-of-line data. When present, the tag's value/offset field
    /// is patched to point to this data appended after all IFDs.
    ool_data: Option<Vec<u8>>,
}

impl SyntheticTag {
    pub(super) fn long(tag: u16, value: u32) -> Self {
        SyntheticTag {
            tag,
            tiff_type: 4, // LONG
            count: 1,
            inline_value: value.to_le_bytes(),
            ool_data: None,
        }
    }

    pub(super) fn short(tag: u16, value: u16) -> Self {
        let mut bytes = [0u8; 4];
        bytes[0..2].copy_from_slice(&value.to_le_bytes());
        SyntheticTag {
            tag,
            tiff_type: 3, // SHORT
            count: 1,
            inline_value: bytes,
            ool_data: None,
        }
    }

    pub(super) fn ascii(tag: u16, text: &str) -> Self {
        let mut data = text.as_bytes().to_vec();
        data.push(0); // null terminator
        SyntheticTag {
            tag,
            tiff_type: 2, // ASCII
            count: data.len() as u32,
            inline_value: [0; 4],
            ool_data: Some(data),
        }
    }

    pub(super) fn bytes(tag: u16, data: Vec<u8>) -> Self {
        SyntheticTag {
            tag,
            tiff_type: 7, // UNDEFINED
            count: data.len() as u32,
            inline_value: [0; 4],
            ool_data: Some(data),
        }
    }
}

/// Build a synthetic classic TIFF file with chained top-level IFDs.
/// Supports both inline and out-of-line tag data.
pub(super) fn build_aperio_tiff(ifds: &[Vec<SyntheticTag>]) -> NamedTempFile {
    let mut buf = Vec::new();

    // TIFF header: little-endian, classic TIFF
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    let first_ifd_offset_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes()); // placeholder

    // First pass: write out-of-line data blocks and record their offsets.
    // We accumulate (ifd_idx, tag_idx, file_offset) tuples.
    let mut ool_offsets: Vec<(usize, usize, u32)> = Vec::new();
    for (ifd_idx, tags) in ifds.iter().enumerate() {
        for (tag_idx, tag) in tags.iter().enumerate() {
            if let Some(data) = &tag.ool_data {
                let offset = buf.len() as u32;
                buf.extend_from_slice(data);
                ool_offsets.push((ifd_idx, tag_idx, offset));
            }
        }
    }

    // Second pass: write IFDs
    let mut ifd_offsets: Vec<u32> = Vec::new();
    let mut next_ifd_patch_positions: Vec<usize> = Vec::new();

    for (ifd_idx, tags) in ifds.iter().enumerate() {
        let ifd_offset = buf.len() as u32;
        ifd_offsets.push(ifd_offset);

        // Sort tags by ID (TIFF spec requirement)
        let mut sorted: Vec<(usize, &SyntheticTag)> = tags.iter().enumerate().collect();
        sorted.sort_by_key(|(_, t)| t.tag);

        let entry_count = sorted.len() as u16;
        buf.extend_from_slice(&entry_count.to_le_bytes());

        for (orig_idx, tag) in &sorted {
            buf.extend_from_slice(&tag.tag.to_le_bytes());
            buf.extend_from_slice(&tag.tiff_type.to_le_bytes());
            buf.extend_from_slice(&tag.count.to_le_bytes());

            if tag.ool_data.is_some() {
                // Find the offset we recorded
                let offset = ool_offsets
                    .iter()
                    .find(|(ii, ti, _)| *ii == ifd_idx && *ti == *orig_idx)
                    .map(|(_, _, o)| *o)
                    .unwrap();
                buf.extend_from_slice(&offset.to_le_bytes());
            } else {
                buf.extend_from_slice(&tag.inline_value);
            }
        }

        // Next IFD offset (classic TIFF: 4 bytes)
        let next_pos = buf.len();
        buf.extend_from_slice(&0u32.to_le_bytes());
        next_ifd_patch_positions.push(next_pos);
    }

    // Patch first IFD offset
    let first_offset = ifd_offsets[0].to_le_bytes();
    buf[first_ifd_offset_pos..first_ifd_offset_pos + 4].copy_from_slice(&first_offset);

    // Chain IFDs
    for i in 0..ifd_offsets.len().saturating_sub(1) {
        let next = ifd_offsets[i + 1].to_le_bytes();
        let pos = next_ifd_patch_positions[i];
        buf[pos..pos + 4].copy_from_slice(&next);
    }

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}
