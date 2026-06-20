use super::super::error::{IfdId, TiffParseError};
use super::ndpi_offsets::fix_offset_ndpi;
use super::*;
use std::collections::HashMap;
use std::path::Path;

// ── TiffType ───────────────────────────────────────────────

#[test]
fn tiff_type_from_u16_known_types() {
    assert_eq!(TiffType::from_u16(1), Some(TiffType::Byte));
    assert_eq!(TiffType::from_u16(2), Some(TiffType::Ascii));
    assert_eq!(TiffType::from_u16(3), Some(TiffType::Short));
    assert_eq!(TiffType::from_u16(4), Some(TiffType::Long));
    assert_eq!(TiffType::from_u16(5), Some(TiffType::Rational));
    assert_eq!(TiffType::from_u16(6), Some(TiffType::SByte));
    assert_eq!(TiffType::from_u16(7), Some(TiffType::Undefined));
    assert_eq!(TiffType::from_u16(8), Some(TiffType::SShort));
    assert_eq!(TiffType::from_u16(9), Some(TiffType::SLong));
    assert_eq!(TiffType::from_u16(10), Some(TiffType::SRational));
    assert_eq!(TiffType::from_u16(11), Some(TiffType::Float));
    assert_eq!(TiffType::from_u16(12), Some(TiffType::Double));
    assert_eq!(TiffType::from_u16(13), Some(TiffType::Ifd));
    assert_eq!(TiffType::from_u16(16), Some(TiffType::Long8));
    assert_eq!(TiffType::from_u16(17), Some(TiffType::SLong8));
    assert_eq!(TiffType::from_u16(18), Some(TiffType::Ifd8));
}

#[test]
fn tiff_type_from_u16_unknown() {
    assert_eq!(TiffType::from_u16(0), None);
    assert_eq!(TiffType::from_u16(14), None);
    assert_eq!(TiffType::from_u16(15), None);
    assert_eq!(TiffType::from_u16(19), None);
    assert_eq!(TiffType::from_u16(255), None);
}

#[test]
fn tiff_type_byte_sizes() {
    assert_eq!(TiffType::Byte.byte_size(), 1);
    assert_eq!(TiffType::Ascii.byte_size(), 1);
    assert_eq!(TiffType::Short.byte_size(), 2);
    assert_eq!(TiffType::Long.byte_size(), 4);
    assert_eq!(TiffType::Rational.byte_size(), 8);
    assert_eq!(TiffType::Float.byte_size(), 4);
    assert_eq!(TiffType::Double.byte_size(), 8);
    assert_eq!(TiffType::Long8.byte_size(), 8);
    assert_eq!(TiffType::Ifd.byte_size(), 4);
    assert_eq!(TiffType::Ifd8.byte_size(), 8);
}

#[test]
fn tiff_type_round_trip() {
    for id in 1..=18 {
        if let Some(tt) = TiffType::from_u16(id) {
            assert!(tt.byte_size() > 0, "type {:?} has zero byte_size", tt);
        }
    }
}

// ── InlineValue ────────────────────────────────────────────

#[test]
fn inline_value_construction_and_access() {
    let val = InlineValue::new(&[1, 2, 3, 4]);
    assert_eq!(val.as_bytes(), &[1, 2, 3, 4]);
}

#[test]
fn inline_value_empty() {
    let val = InlineValue::new(&[]);
    assert_eq!(val.as_bytes().len(), 0);
}

#[test]
fn inline_value_max_capacity() {
    let data = [0xFFu8; 12];
    let val = InlineValue::new(&data);
    assert_eq!(val.as_bytes().len(), 12);
    assert_eq!(val.as_bytes(), &data);
}

#[test]
#[should_panic(expected = "exceeds 12 bytes")]
fn inline_value_rejects_oversized() {
    let _ = InlineValue::new(&[0u8; 13]);
}

// ── Endian ─────────────────────────────────────────────────

#[test]
fn endian_equality() {
    assert_eq!(Endian::Little, Endian::Little);
    assert_eq!(Endian::Big, Endian::Big);
    assert_ne!(Endian::Little, Endian::Big);
}

// ── TagEntry / TagValue ────────────────────────────────────

#[test]
fn tag_entry_inline_construction() {
    let entry = TagEntry::new_inline(TiffType::Short, 1, &[0x00, 0x01]);
    assert_eq!(entry.tiff_type, TiffType::Short);
    assert_eq!(entry.count, 1);
    match &entry.value {
        TagValue::Inline(v) => assert_eq!(v.as_bytes(), &[0x00, 0x01]),
        TagValue::Lazy { .. } => panic!("expected Inline"),
    }
}

#[test]
fn tag_entry_lazy_construction() {
    let entry = TagEntry::new_lazy(TiffType::Long, 100, 4096, 400);
    assert_eq!(entry.tiff_type, TiffType::Long);
    assert_eq!(entry.count, 100);
    match &entry.value {
        TagValue::Lazy {
            offset, byte_len, ..
        } => {
            assert_eq!(*offset, 4096);
            assert_eq!(*byte_len, 400);
        }
        TagValue::Inline(_) => panic!("expected Lazy"),
    }
}

#[test]
fn tag_entry_decoded_oncelocks_start_empty() {
    let entry = TagEntry::new_inline(TiffType::Long, 1, &[0, 0, 0, 1]);
    assert!(entry.decoded_u64.get().is_none());
}

#[test]
fn ifd_construction() {
    let mut tags = HashMap::new();
    tags.insert(256, TagEntry::new_inline(TiffType::Long, 1, &[0, 0, 4, 0]));
    let ifd = Ifd {
        id: IfdId(1024),
        offset: 1024,
        tags,
        sub_ifds: vec![],
    };
    assert_eq!(ifd.id, IfdId(1024));
    assert_eq!(ifd.tags.len(), 1);
    assert!(ifd.tags.contains_key(&256));
}

// ── Test helpers for synthetic TIFFs ───────────────────────

use std::io::Write;

/// Write a u16 in the given endianness.
fn write_u16(buf: &mut Vec<u8>, endian: Endian, val: u16) {
    match endian {
        Endian::Little => buf.extend_from_slice(&val.to_le_bytes()),
        Endian::Big => buf.extend_from_slice(&val.to_be_bytes()),
    }
}

/// Write a u32 in the given endianness.
fn write_u32(buf: &mut Vec<u8>, endian: Endian, val: u32) {
    match endian {
        Endian::Little => buf.extend_from_slice(&val.to_le_bytes()),
        Endian::Big => buf.extend_from_slice(&val.to_be_bytes()),
    }
}

/// Write a u64 in the given endianness.
fn write_u64(buf: &mut Vec<u8>, endian: Endian, val: u64) {
    match endian {
        Endian::Little => buf.extend_from_slice(&val.to_le_bytes()),
        Endian::Big => buf.extend_from_slice(&val.to_be_bytes()),
    }
}

/// A synthetic IFD entry for test data construction.
struct SyntheticEntry {
    tag: u16,
    tiff_type: u16,
    count: u64,
    /// Inline data (used when total_bytes <= slot_size).
    /// If None and data is out-of-line, `out_of_line_data` is used.
    inline_data: Option<Vec<u8>>,
    /// Out-of-line data to be written at a computed offset.
    out_of_line_data: Option<Vec<u8>>,
}

/// Build a minimal classic TIFF with one IFD.
/// Returns (bytes, ifd_offset).
fn make_classic_tiff_single(endian: Endian, entries: &[SyntheticEntry]) -> Vec<u8> {
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
fn make_bigtiff_single(endian: Endian, entries: &[SyntheticEntry]) -> Vec<u8> {
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
fn write_tiff_tempfile(data: &[u8]) -> tempfile::NamedTempFile {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    tmp.write_all(data).unwrap();
    tmp.flush().unwrap();
    tmp
}

// ── Header parsing tests ──────────────────────────────────

#[test]
fn parse_classic_le_header() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4, // LONG
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0]), // 1024 LE
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.endian(), Endian::Little);
    assert!(!container.is_bigtiff());
    assert!(!container.is_ndpi());
}

#[test]
fn parse_classic_be_header() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 0, 4, 0]), // 1024 BE
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Big, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.endian(), Endian::Big);
    assert!(!container.is_bigtiff());
}

#[test]
fn parse_bigtiff_le_header() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 16, // LONG8
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0, 0, 0, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_bigtiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.endian(), Endian::Little);
    assert!(container.is_bigtiff());
}

#[test]
fn parse_bigtiff_be_header() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 16,
        count: 1,
        inline_data: Some(vec![0, 0, 0, 0, 0, 0, 4, 0]),
        out_of_line_data: None,
    }];
    let data = make_bigtiff_single(Endian::Big, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.endian(), Endian::Big);
    assert!(container.is_bigtiff());
}

#[test]
fn reject_invalid_magic() {
    let mut data = vec![b'I', b'I'];
    data.extend_from_slice(&99u16.to_le_bytes()); // bad magic
    data.extend_from_slice(&8u32.to_le_bytes()); // dummy offset
    let tmp = write_tiff_tempfile(&data);
    let result = TiffContainer::open(tmp.path());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, TiffParseError::Structure(_)),
        "got: {:?}",
        err
    );
}

#[test]
fn reject_bad_bigtiff_offset_size() {
    let mut data = Vec::new();
    data.extend_from_slice(b"II");
    data.extend_from_slice(&43u16.to_le_bytes()); // BigTIFF magic
    data.extend_from_slice(&4u16.to_le_bytes()); // wrong offset size (should be 8)
    data.extend_from_slice(&0u16.to_le_bytes()); // reserved
    data.extend_from_slice(&16u64.to_le_bytes()); // first IFD offset
    let tmp = write_tiff_tempfile(&data);
    let result = TiffContainer::open(tmp.path());
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), TiffParseError::Structure(_)));
}

#[test]
fn pread_bounds_check_rejects_out_of_bounds() {
    // Create a minimal valid TIFF so TiffContainer::open() succeeds
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 1, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let file_len = data.len() as u64;
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    // Try to read beyond file end
    let result = container.pread(file_len - 2, 10);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), TiffParseError::Bounds { .. }));
}

#[test]
fn pread_offset_overflow_rejected() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 1, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    // Offset that would overflow u64
    let result = container.pread(u64::MAX, 10);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), TiffParseError::Bounds { .. }));
}

// ── Multi-IFD test helper ─────────────────────────────────

/// Build a classic TIFF with two chained IFDs, each with the given entries.
fn make_classic_tiff_two_ifds(
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

#[test]
fn single_ifd_parsed() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4, // LONG
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.ifd_count(), 1);
    assert_eq!(container.top_ifds().len(), 1);
    let ifd = container.ifd_by_id(container.top_ifds()[0]).unwrap();
    assert!(ifd.tags.contains_key(&tags::IMAGE_WIDTH));
}

#[test]
fn two_chained_ifds_parsed() {
    let e1 = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0]),
        out_of_line_data: None,
    }];
    let e2 = vec![SyntheticEntry {
        tag: tags::IMAGE_LENGTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 3, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_two_ifds(Endian::Little, &e1, &e2);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.ifd_count(), 2);
    assert_eq!(container.top_ifds().len(), 2);
    // IFDs should have different IDs (offsets)
    assert_ne!(container.top_ifds()[0], container.top_ifds()[1]);
}

#[test]
fn ifd_chain_loop_detected() {
    let mut data = Vec::new();
    data.extend_from_slice(b"II");
    write_u16(&mut data, Endian::Little, 42);
    // First IFD at offset 8
    write_u32(&mut data, Endian::Little, 8);
    // IFD with 0 entries
    write_u16(&mut data, Endian::Little, 0);
    // Next IFD offset points back to 8 (loop!)
    write_u32(&mut data, Endian::Little, 8);

    let tmp = write_tiff_tempfile(&data);
    let result = TiffContainer::open(tmp.path());
    assert!(result.is_err());
    match result.unwrap_err() {
        TiffParseError::Structure(msg) => assert!(msg.contains("loop"), "got: {}", msg),
        other => panic!("expected Structure, got: {:?}", other),
    }
}

#[test]
fn empty_ifd_accepted() {
    let mut data = Vec::new();
    data.extend_from_slice(b"II");
    write_u16(&mut data, Endian::Little, 42);
    write_u32(&mut data, Endian::Little, 8); // IFD at offset 8
    write_u16(&mut data, Endian::Little, 0); // 0 entries
    write_u32(&mut data, Endian::Little, 0); // no next IFD

    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.ifd_count(), 1);
    let ifd = container.ifd_by_id(container.top_ifds()[0]).unwrap();
    assert_eq!(ifd.tags.len(), 0);
}

#[test]
fn unknown_type_id_skipped() {
    // Create an entry with type ID 99 (unknown) — should be skipped
    let entries = vec![
        SyntheticEntry {
            tag: 999,
            tiff_type: 99, // unknown
            count: 1,
            inline_data: Some(vec![0, 0, 0, 0]),
            out_of_line_data: None,
        },
        SyntheticEntry {
            tag: tags::IMAGE_WIDTH,
            tiff_type: 4, // LONG
            count: 1,
            inline_data: Some(vec![0, 4, 0, 0]),
            out_of_line_data: None,
        },
    ];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd = container.ifd_by_id(container.top_ifds()[0]).unwrap();
    // Unknown type entry skipped, only IMAGE_WIDTH present
    assert!(!ifd.tags.contains_key(&999));
    assert!(ifd.tags.contains_key(&tags::IMAGE_WIDTH));
}

#[test]
fn ndpi_detection_via_tag_65420() {
    // NDPI uses 8-byte next-IFD pointers, so we manually construct the data.
    let mut ndpi_data = Vec::new();
    ndpi_data.extend_from_slice(b"II");
    write_u16(&mut ndpi_data, Endian::Little, 42);
    write_u32(&mut ndpi_data, Endian::Little, 8); // first IFD at 8

    // IFD: 2 entries
    write_u16(&mut ndpi_data, Endian::Little, 2);
    // Entry 1: NDPI marker tag
    write_u16(&mut ndpi_data, Endian::Little, tags::NDPI_MARKER);
    write_u16(&mut ndpi_data, Endian::Little, 4); // LONG
    write_u32(&mut ndpi_data, Endian::Little, 1);
    ndpi_data.extend_from_slice(&1u32.to_le_bytes());
    // Entry 2: IMAGE_WIDTH
    write_u16(&mut ndpi_data, Endian::Little, tags::IMAGE_WIDTH);
    write_u16(&mut ndpi_data, Endian::Little, 4); // LONG
    write_u32(&mut ndpi_data, Endian::Little, 1);
    ndpi_data.extend_from_slice(&1024u32.to_le_bytes());
    // Next IFD offset: 8 bytes for NDPI (value = 0)
    ndpi_data.extend_from_slice(&0u64.to_le_bytes());

    let tmp = write_tiff_tempfile(&ndpi_data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert!(container.is_ndpi());
}

#[test]
fn ifd_by_id_not_found() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 1, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let result = container.ifd_by_id(IfdId(999999));
    assert!(matches!(
        result.unwrap_err(),
        TiffParseError::IfdNotFound(_)
    ));
}

// ── NDPI offset fixup tests ───────────────────────────────

#[test]
fn ndpi_fixup_low_offset_unchanged() {
    // When both diroff and offset are in the low 4GB, result should be offset
    let result = fix_offset_ndpi(1000, 500);
    assert_eq!(result, 500);
}

#[test]
fn ndpi_fixup_high_diroff_reconstructs() {
    // diroff at 5GB, offset stored as low 32 bits of 4.5GB
    let diroff: u64 = 5 * 1024 * 1024 * 1024; // 5 GB
    let real_offset: u64 = 4 * 1024 * 1024 * 1024 + 500_000_000; // 4.5 GB
    let stored_offset = real_offset & u64::from(u32::MAX); // low 32 bits
    let result = fix_offset_ndpi(diroff, stored_offset);
    assert_eq!(result, real_offset);
}

#[test]
fn ndpi_fixup_result_below_diroff() {
    // The fixup should always produce a result <= diroff
    // (data referenced by an IFD should precede it)
    let diroff: u64 = 6 * 1024 * 1024 * 1024;
    let stored_offset: u64 = 100;
    let result = fix_offset_ndpi(diroff, stored_offset);
    assert!(result <= diroff, "result {} > diroff {}", result, diroff);
}

#[test]
fn ndpi_fixup_zero_diroff() {
    // When diroff is 0, the heuristic clamps: result >= diroff triggers
    // saturating_sub(4GB) which floors to 0.
    let result = fix_offset_ndpi(0, 12345);
    assert_eq!(result, 0);
}

#[test]
fn opens_wrapped_first_ifd_ndpi_when_corpus_is_available() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let path = workspace_root.join("downloads/openslide-testdata/Hamamatsu/Hamamatsu-1.ndpi");
    if !path.exists() {
        return;
    }

    let container = TiffContainer::open(&path).expect("open wrapped-offset NDPI");
    assert!(container.is_ndpi());
    assert!(!container.top_ifds().is_empty());
}

// ── SubIFD test helpers ───────────────────────────────────

/// Build a classic TIFF with one main IFD that has a SUB_IFDS tag
/// pointing to one SubIFD.
fn make_classic_tiff_with_subifd(endian: Endian) -> Vec<u8> {
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

#[test]
fn open_does_not_materialize_sub_ifds_eagerly() {
    let data = make_classic_tiff_with_subifd(Endian::Little);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();

    assert_eq!(container.top_ifds().len(), 1);
    assert_eq!(container.ifd_count(), 1);

    let main_ifd = container.ifd_by_id(container.top_ifds()[0]).unwrap();
    assert!(main_ifd.sub_ifds.is_empty());
}

#[test]
fn sub_ifd_parsed() {
    let data = make_classic_tiff_with_subifd(Endian::Little);
    let tmp = write_tiff_tempfile(&data);
    let mut container = TiffContainer::open(tmp.path()).unwrap();

    let main_id = container.top_ifds()[0];
    container
        .materialize_sub_ifds(main_id, 4)
        .expect("materialize subifds");

    assert_eq!(container.top_ifds().len(), 1);
    assert_eq!(container.ifd_count(), 2); // main + sub

    let main_ifd = container.ifd_by_id(main_id).unwrap();
    assert_eq!(main_ifd.sub_ifds.len(), 1);

    let sub_ifd = container.ifd_by_id(main_ifd.sub_ifds[0]).unwrap();
    assert!(sub_ifd.tags.contains_key(&tags::IMAGE_LENGTH));
}

#[test]
fn sub_ifd_nested_depth_2() {
    // Build a TIFF with main IFD -> SubIFD -> sub-SubIFD
    let endian = Endian::Little;
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    write_u16(&mut buf, endian, 42);
    write_u32(&mut buf, endian, 8); // main IFD at 8

    // Main IFD at 8: 1 entry (SUB_IFDS)
    // Size: 2 + 12 + 4 = 18, so SubIFD1 at 8+18 = 26
    write_u16(&mut buf, endian, 1);
    write_u16(&mut buf, endian, tags::SUB_IFDS);
    write_u16(&mut buf, endian, 4); // LONG
    write_u32(&mut buf, endian, 1);
    write_u32(&mut buf, endian, 26); // SubIFD1 at 26
    write_u32(&mut buf, endian, 0); // next IFD = 0

    // SubIFD1 at 26: 1 entry (SUB_IFDS) pointing to SubIFD2
    // Size: 2 + 12 + 4 = 18, so SubIFD2 at 26+18 = 44
    assert_eq!(buf.len(), 26);
    write_u16(&mut buf, endian, 1);
    write_u16(&mut buf, endian, tags::SUB_IFDS);
    write_u16(&mut buf, endian, 4); // LONG
    write_u32(&mut buf, endian, 1);
    write_u32(&mut buf, endian, 44); // SubIFD2 at 44
    write_u32(&mut buf, endian, 0); // next IFD = 0

    // SubIFD2 at 44: 1 entry (IMAGE_WIDTH)
    assert_eq!(buf.len(), 44);
    write_u16(&mut buf, endian, 1);
    write_u16(&mut buf, endian, tags::IMAGE_WIDTH);
    write_u16(&mut buf, endian, 4); // LONG
    write_u32(&mut buf, endian, 1);
    write_u32(&mut buf, endian, 512);
    write_u32(&mut buf, endian, 0); // next IFD = 0

    let tmp = write_tiff_tempfile(&buf);
    let mut container = TiffContainer::open(tmp.path()).unwrap();
    let main_id = container.top_ifds()[0];
    container
        .materialize_sub_ifds(main_id, 4)
        .expect("materialize nested subifds");

    assert_eq!(container.ifd_count(), 3); // main + sub1 + sub2
    let main_ifd = container.ifd_by_id(main_id).unwrap();
    assert_eq!(main_ifd.sub_ifds.len(), 1);
    let sub1 = container.ifd_by_id(main_ifd.sub_ifds[0]).unwrap();
    assert_eq!(sub1.sub_ifds.len(), 1);
    let sub2 = container.ifd_by_id(sub1.sub_ifds[0]).unwrap();
    assert!(sub2.tags.contains_key(&tags::IMAGE_WIDTH));
}

#[test]
fn sub_ifd_duplicate_offset_dedup() {
    // Two entries in SUB_IFDS tag pointing to the same offset
    let endian = Endian::Little;
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    write_u16(&mut buf, endian, 42);
    write_u32(&mut buf, endian, 8);

    // Main IFD at 8: 1 entry (SUB_IFDS with count=2, inline since 2*4=8 > 4 -> OOL)
    // Actually 2 LONGs = 8 bytes > 4 byte slot -> out-of-line
    // Main IFD size: 2 + 12 + 4 = 18, OOL data at 8+18 = 26
    // SubIFD at 26+8 = 34
    write_u16(&mut buf, endian, 1);
    write_u16(&mut buf, endian, tags::SUB_IFDS);
    write_u16(&mut buf, endian, 4); // LONG
    write_u32(&mut buf, endian, 2); // count=2
    write_u32(&mut buf, endian, 26); // OOL data offset
    write_u32(&mut buf, endian, 0); // next IFD = 0

    // OOL data at 26: two offsets both pointing to 34
    assert_eq!(buf.len(), 26);
    write_u32(&mut buf, endian, 34);
    write_u32(&mut buf, endian, 34); // duplicate!

    // SubIFD at 34: 1 entry
    assert_eq!(buf.len(), 34);
    write_u16(&mut buf, endian, 1);
    write_u16(&mut buf, endian, tags::IMAGE_WIDTH);
    write_u16(&mut buf, endian, 4);
    write_u32(&mut buf, endian, 1);
    write_u32(&mut buf, endian, 256);
    write_u32(&mut buf, endian, 0);

    let tmp = write_tiff_tempfile(&buf);
    let mut container = TiffContainer::open(tmp.path()).unwrap();
    let main_id = container.top_ifds()[0];
    container
        .materialize_sub_ifds(main_id, 4)
        .expect("materialize duplicate subifds");

    // Only 2 unique IFDs (main + one SubIFD), despite two references
    assert_eq!(container.ifd_count(), 2);

    let main_ifd = container.ifd_by_id(main_id).unwrap();
    // Both references stored (preserves topology)
    assert_eq!(main_ifd.sub_ifds.len(), 2);
    assert_eq!(main_ifd.sub_ifds[0], main_ifd.sub_ifds[1]);
}

#[test]
fn sub_ifd_depth_limit() {
    // Build 5 levels of nested SubIFDs (limit is 4)
    let endian = Endian::Little;
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    write_u16(&mut buf, endian, 42);
    write_u32(&mut buf, endian, 8);

    // Each IFD: 2 (count) + 12 (1 entry) + 4 (next) = 18 bytes
    let ifd_size = 18u32;
    let mut current_offset = 8u32;

    for i in 0..6 {
        let is_last = i == 5;
        assert_eq!(buf.len(), current_offset as usize);
        write_u16(&mut buf, endian, 1); // 1 entry

        if is_last {
            // Last IFD has IMAGE_WIDTH instead of SUB_IFDS
            write_u16(&mut buf, endian, tags::IMAGE_WIDTH);
            write_u16(&mut buf, endian, 4);
            write_u32(&mut buf, endian, 1);
            write_u32(&mut buf, endian, 100);
        } else {
            let next_sub = current_offset + ifd_size;
            write_u16(&mut buf, endian, tags::SUB_IFDS);
            write_u16(&mut buf, endian, 4); // LONG
            write_u32(&mut buf, endian, 1);
            write_u32(&mut buf, endian, next_sub);
        }
        write_u32(&mut buf, endian, 0); // no next IFD in chain
        current_offset += ifd_size;
    }

    let tmp = write_tiff_tempfile(&buf);
    let mut container = TiffContainer::open(tmp.path()).unwrap();
    let main_id = container.top_ifds()[0];
    let result = container.materialize_sub_ifds(main_id, 4);
    // Should fail because depth exceeds 4
    assert!(result.is_err());
    match result.unwrap_err() {
        TiffParseError::Structure(msg) => {
            assert!(msg.contains("depth"), "got: {}", msg);
        }
        other => panic!("expected Structure, got: {:?}", other),
    }
}

#[test]
fn sub_ifd_cross_reference_preserved() {
    let data = make_classic_tiff_with_subifd(Endian::Little);
    let tmp = write_tiff_tempfile(&data);
    let mut container = TiffContainer::open(tmp.path()).unwrap();
    container
        .materialize_all_sub_ifds(4)
        .expect("materialize all subifds");

    let main_ifd = container.ifd_by_id(container.top_ifds()[0]).unwrap();
    let sub_id = main_ifd.sub_ifds[0];

    // SubIFD accessible via flat arena lookup (O(1))
    let sub_ifd = container.ifd_by_id(sub_id).unwrap();
    assert_eq!(sub_ifd.id, sub_id);
    assert_eq!(sub_ifd.offset, sub_id.0);
}

// ── Lazy resolution tests ─────────────────────────────────

#[test]
fn resolve_inline_tag_returns_bytes() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4, // LONG
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0]), // 1024 LE
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();

    let ifd_id = container.top_ifds()[0];
    let bytes = container.resolve_tag(ifd_id, tags::IMAGE_WIDTH).unwrap();
    assert_eq!(bytes, &[0, 4, 0, 0]);
}

#[test]
fn resolve_lazy_tag_triggers_io() {
    // Create a tag with out-of-line data (>4 bytes for classic)
    let ool_data: Vec<u8> = vec![1, 0, 0, 0, 2, 0, 0, 0]; // two LONGs
    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 4, // LONG
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool_data.clone()),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();

    let ifd_id = container.top_ifds()[0];
    let bytes = container.resolve_tag(ifd_id, tags::TILE_OFFSETS).unwrap();
    assert_eq!(bytes, &ool_data);
}

#[test]
fn resolve_lazy_tag_cached() {
    let ool_data: Vec<u8> = vec![1, 0, 0, 0, 2, 0, 0, 0];
    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 4,
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool_data.clone()),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();

    let ifd_id = container.top_ifds()[0];
    let bytes1 = container.resolve_tag(ifd_id, tags::TILE_OFFSETS).unwrap();
    let bytes2 = container.resolve_tag(ifd_id, tags::TILE_OFFSETS).unwrap();
    // Same slice returned (same OnceLock)
    assert_eq!(bytes1.as_ptr(), bytes2.as_ptr());
}

#[test]
fn resolve_tag_not_found() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 1, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();

    let ifd_id = container.top_ifds()[0];
    let result = container.resolve_tag(ifd_id, tags::TILE_OFFSETS);
    assert!(matches!(
        result.unwrap_err(),
        TiffParseError::TagNotFound { .. }
    ));
}

#[test]
fn inline_classification_correct() {
    // 1 LONG = 4 bytes -> inline in classic TIFF (slot_size=4)
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let ifd = container.ifd_by_id(ifd_id).unwrap();
    let entry = ifd.tags.get(&tags::IMAGE_WIDTH).unwrap();
    assert!(matches!(entry.value, TagValue::Inline(_)));
}

// ── Typed scalar accessor tests ───────────────────────────

#[test]
fn get_u32_from_short() {
    let entries = vec![SyntheticEntry {
        tag: tags::BITS_PER_SAMPLE,
        tiff_type: 3, // SHORT
        count: 1,
        inline_data: Some(vec![8, 0, 0, 0]), // 8 LE
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    assert_eq!(container.get_u32(ifd_id, tags::BITS_PER_SAMPLE).unwrap(), 8);
}

#[test]
fn get_u32_from_long() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4, // LONG
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0]), // 1024 LE
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    assert_eq!(container.get_u32(ifd_id, tags::IMAGE_WIDTH).unwrap(), 1024);
}

#[test]
fn get_u64_from_long8() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 16, // LONG8
        count: 1,
        inline_data: Some(vec![0, 0, 0, 1, 0, 0, 0, 0]), // 16777216 LE
        out_of_line_data: None,
    }];
    let data = make_bigtiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    assert_eq!(
        container.get_u64(ifd_id, tags::IMAGE_WIDTH).unwrap(),
        16777216
    );
}

#[test]
fn get_f64_from_rational() {
    let mut rational_bytes = Vec::new();
    rational_bytes.extend_from_slice(&72u32.to_le_bytes());
    rational_bytes.extend_from_slice(&1u32.to_le_bytes());

    let entries = vec![SyntheticEntry {
        tag: 282,
        tiff_type: 5, // RATIONAL
        count: 1,
        inline_data: None,
        out_of_line_data: Some(rational_bytes),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let val = container.get_f64(ifd_id, 282).unwrap();
    assert!((val - 72.0).abs() < f64::EPSILON);
}

#[test]
fn get_f64_from_float() {
    let float_val: f32 = std::f32::consts::PI;
    let entries = vec![SyntheticEntry {
        tag: 500,      // arbitrary tag
        tiff_type: 11, // FLOAT
        count: 1,
        inline_data: Some(float_val.to_le_bytes().to_vec()),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let val = container.get_f64(ifd_id, 500).unwrap();
    assert!(
        (val - f64::from(std::f32::consts::PI)).abs() < 0.001,
        "got: {}",
        val
    );
}

#[test]
fn get_string_ascii() {
    let text = b"Hello\0";
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_DESCRIPTION,
        tiff_type: 2, // ASCII
        count: text.len() as u64,
        inline_data: None,
        out_of_line_data: Some(text.to_vec()),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let s = container
        .get_string(ifd_id, tags::IMAGE_DESCRIPTION)
        .unwrap();
    assert_eq!(s, "Hello");
}

#[test]
fn get_u32_type_mismatch() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_DESCRIPTION,
        tiff_type: 2, // ASCII
        count: 4,
        inline_data: Some(b"foo\0".to_vec()),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let result = container.get_u32(ifd_id, tags::IMAGE_DESCRIPTION);
    assert!(matches!(
        result.unwrap_err(),
        TiffParseError::InvalidTag { .. }
    ));
}

// ── Typed array accessor tests ────────────────────────────

#[test]
fn get_u64_array_from_long() {
    // Two LONGs = 8 bytes -> out-of-line for classic
    let mut ool = Vec::new();
    ool.extend_from_slice(&100u32.to_le_bytes());
    ool.extend_from_slice(&200u32.to_le_bytes());

    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 4, // LONG
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let arr = container.get_u64_array(ifd_id, tags::TILE_OFFSETS).unwrap();
    assert_eq!(arr, &[100, 200]);
}

#[test]
fn get_u64_array_from_long8() {
    let mut ool = Vec::new();
    ool.extend_from_slice(&5_000_000_000u64.to_le_bytes());
    ool.extend_from_slice(&6_000_000_000u64.to_le_bytes());

    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 16, // LONG8
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool),
    }];
    let data = make_bigtiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let arr = container.get_u64_array(ifd_id, tags::TILE_OFFSETS).unwrap();
    assert_eq!(arr, &[5_000_000_000, 6_000_000_000]);
}

#[test]
fn get_u64_array_cached_pointer_equality() {
    let mut ool = Vec::new();
    ool.extend_from_slice(&100u32.to_le_bytes());
    ool.extend_from_slice(&200u32.to_le_bytes());

    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 4,
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];

    let arr1 = container.get_u64_array(ifd_id, tags::TILE_OFFSETS).unwrap();
    let arr2 = container.get_u64_array(ifd_id, tags::TILE_OFFSETS).unwrap();
    // Same pointer — cached, not re-decoded
    assert_eq!(arr1.as_ptr(), arr2.as_ptr());
}

#[test]
fn get_u64_array_type_mismatch() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_DESCRIPTION,
        tiff_type: 2, // ASCII
        count: 4,
        inline_data: Some(b"foo\0".to_vec()),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let result = container.get_u64_array(ifd_id, tags::IMAGE_DESCRIPTION);
    assert!(matches!(
        result.unwrap_err(),
        TiffParseError::InvalidTag { .. }
    ));
}
