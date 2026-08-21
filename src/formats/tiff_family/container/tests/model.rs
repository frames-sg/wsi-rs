use super::*;

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
