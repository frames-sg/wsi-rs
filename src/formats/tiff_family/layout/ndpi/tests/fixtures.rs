use super::super::*;
use std::io::Write;
use tempfile::NamedTempFile;

use crate::formats::tiff_family::test_support::encode_test_jpeg;

pub(super) fn synthetic_dri_420_jpeg_header() -> Vec<u8> {
    vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xDD, 0x00, 0x04, 0x00, 0x0A, // DRI = 10 MCUs
        0xFF, 0xC0, 0x00, 0x11, // SOF0
        0x08, // precision
        0x00, 0x80, // height = 128
        0x01, 0x00, // width = 256
        0x03, // components
        0x01, 0x22, 0x00, // Y: H=2, V=2
        0x02, 0x11, 0x01, // Cb: H=1, V=1
        0x03, 0x11, 0x01, // Cr: H=1, V=1
        0xFF, 0xDA, 0x00, 0x0C, // SOS
        0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00,
    ]
}

/// Build a minimal TIFF file in memory with the given IFDs.
/// Each IFD is a list of (tag, type_id, count, value_bytes).
/// Supports only inline tags (value fits in 4 bytes) for simplicity.
/// Returns a NamedTempFile containing the TIFF data.
#[allow(clippy::type_complexity)]
pub(super) fn build_synthetic_tiff(
    ifds: &[Vec<(u16, u16, u32, [u8; 4])>],
    ndpi: bool,
) -> NamedTempFile {
    let mut buf = Vec::new();

    // TIFF header: little-endian, classic TIFF
    buf.extend_from_slice(b"II"); // byte order
    buf.extend_from_slice(&42u16.to_le_bytes()); // magic
                                                 // First IFD offset -- we'll fill this in later
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes());

    // Build IFDs sequentially
    let mut ifd_offsets = Vec::new();
    let mut next_ifd_patch_positions = Vec::new();

    for (ifd_idx, tags) in ifds.iter().enumerate() {
        let ifd_offset = buf.len() as u32;
        ifd_offsets.push(ifd_offset);

        // Inject NDPI marker tag (65420) into first IFD if requested
        let mut all_tags = tags.clone();
        if ndpi && ifd_idx == 0 {
            all_tags.push((65420, 4, 1, [1, 0, 0, 0])); // LONG, count=1, value=1
        }

        // Sort tags by ID (TIFF requirement)
        all_tags.sort_by_key(|t| t.0);

        let entry_count = all_tags.len() as u16;
        buf.extend_from_slice(&entry_count.to_le_bytes());

        for (tag_id, type_id, count, value) in &all_tags {
            buf.extend_from_slice(&tag_id.to_le_bytes());
            buf.extend_from_slice(&type_id.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
            buf.extend_from_slice(value);
        }

        // Next IFD offset -- placeholder, will patch
        let next_pos = buf.len();
        if ndpi {
            // NDPI uses 8-byte next-IFD pointers
            buf.extend_from_slice(&0u64.to_le_bytes());
        } else {
            buf.extend_from_slice(&0u32.to_le_bytes());
        }
        next_ifd_patch_positions.push((next_pos, ndpi));
    }

    // Patch first IFD offset
    let offset_bytes = ifd_offsets[0].to_le_bytes();
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&offset_bytes);

    // Patch next-IFD pointers to chain them
    for i in 0..ifd_offsets.len() - 1 {
        let (patch_pos, is_ndpi) = next_ifd_patch_positions[i];
        let next_offset = ifd_offsets[i + 1];
        if is_ndpi {
            let bytes = (next_offset as u64).to_le_bytes();
            buf[patch_pos..patch_pos + 8].copy_from_slice(&bytes);
        } else {
            let bytes = next_offset.to_le_bytes();
            buf[patch_pos..patch_pos + 4].copy_from_slice(&bytes);
        }
    }

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

/// Helper: create a LONG tag value (type_id=4, count=1).
pub(super) fn long_tag(tag: u16, value: u32) -> (u16, u16, u32, [u8; 4]) {
    (tag, 4, 1, value.to_le_bytes())
}

/// Helper: create a SHORT tag value (type_id=3, count=1).
pub(super) fn short_tag(tag: u16, value: u16) -> (u16, u16, u32, [u8; 4]) {
    let mut inline = [0u8; 4];
    inline[..2].copy_from_slice(&value.to_le_bytes());
    (tag, 3, 1, inline)
}

/// Helper: create a FLOAT tag value (type_id=11, count=1).
pub(super) fn float_tag(tag: u16, value: f32) -> (u16, u16, u32, [u8; 4]) {
    (tag, 11, 1, value.to_le_bytes())
}

/// Build a synthetic NDPI TIFF with embedded strip payloads at valid offsets.
/// Each entry is (width, height, source_lens, focal_plane, compression_tag).
pub(super) fn build_ndpi_with_strips(
    entries: &[(u32, u32, f32, i32, u32)],
    macro_jpeg_tables: Option<[u8; 4]>,
) -> NamedTempFile {
    let mut strip_blocks: Vec<Vec<u8>> = Vec::new();
    for &(w, h, _, _, compression_tag) in entries {
        let actual_w = w.min(64);
        let actual_h = h.min(64);
        let strip_data = if compression_tag == 1 {
            vec![0u8; actual_w as usize * actual_h as usize * 3]
        } else {
            let rgb = image::RgbImage::new(actual_w, actual_h);
            encode_test_jpeg(&rgb)
        };
        strip_blocks.push(strip_data);
    }

    let mut buf = Vec::new();

    // TIFF header
    buf.extend_from_slice(b"II"); // little-endian
    buf.extend_from_slice(&42u16.to_le_bytes()); // classic TIFF
    let first_ifd_offset_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes()); // first IFD offset placeholder

    // Write JPEG data blocks and remember their offsets
    let mut strip_offsets: Vec<u32> = Vec::new();
    let mut strip_byte_counts: Vec<u32> = Vec::new();
    for strip_data in &strip_blocks {
        strip_offsets.push(buf.len() as u32);
        strip_byte_counts.push(strip_data.len() as u32);
        buf.extend_from_slice(strip_data);
    }

    // Write IFDs
    let mut ifd_offsets: Vec<u32> = Vec::new();
    let mut next_ifd_patch_positions: Vec<usize> = Vec::new();

    for (i, &(w, h, lens, focal, compression_tag)) in entries.iter().enumerate() {
        let ifd_offset = buf.len() as u32;
        ifd_offsets.push(ifd_offset);

        let mut tags: Vec<(u16, u16, u32, [u8; 4])> = vec![
            long_tag(256, w),                                     // IMAGE_WIDTH
            long_tag(257, h),                                     // IMAGE_LENGTH
            short_tag(tags::COMPRESSION, compression_tag as u16), // COMPRESSION
            long_tag(273, strip_offsets[i]),                      // STRIP_OFFSETS
            long_tag(279, strip_byte_counts[i]),                  // STRIP_BYTE_COUNTS
            float_tag(NDPI_SOURCELENS, lens),                     // SOURCELENS
        ];

        // Add FOCAL_PLANE only if non-zero
        if focal != 0 {
            tags.push(float_tag(NDPI_FOCAL_PLANE, focal as f32));
        }
        if lens == -1.0 && compression_tag == 7 {
            if let Some(tables) = macro_jpeg_tables {
                tags.push((tags::JPEG_TABLES, 1, tables.len() as u32, tables));
            }
        }

        // Add NDPI marker tag to first IFD
        if i == 0 {
            tags.push(long_tag(65420, 1)); // NDPI marker
        }

        tags.sort_by_key(|t| t.0);

        let entry_count = tags.len() as u16;
        buf.extend_from_slice(&entry_count.to_le_bytes());

        for (tag_id, type_id, count, value) in &tags {
            buf.extend_from_slice(&tag_id.to_le_bytes());
            buf.extend_from_slice(&type_id.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
            buf.extend_from_slice(value);
        }

        // NDPI 8-byte next-IFD pointer
        let next_pos = buf.len();
        buf.extend_from_slice(&0u64.to_le_bytes());
        next_ifd_patch_positions.push(next_pos);
    }

    // Patch first IFD offset
    buf[first_ifd_offset_pos..first_ifd_offset_pos + 4]
        .copy_from_slice(&ifd_offsets[0].to_le_bytes());

    // Chain IFDs
    for i in 0..ifd_offsets.len() - 1 {
        let next = ifd_offsets[i + 1] as u64;
        let pos = next_ifd_patch_positions[i];
        buf[pos..pos + 8].copy_from_slice(&next.to_le_bytes());
    }

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

/// Build a synthetic NDPI TIFF with JPEG-compressed strips.
/// Each entry is (width, height, source_lens, focal_plane).
pub(super) fn build_ndpi_with_jpeg_strips(entries: &[(u32, u32, f32, i32)]) -> NamedTempFile {
    let entries_with_compression: Vec<_> = entries
        .iter()
        .map(|&(w, h, lens, focal)| (w, h, lens, focal, 7u32))
        .collect();
    build_ndpi_with_strips(&entries_with_compression, None)
}
