use super::*;

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
    // An entry with unknown type ID 99 must be skipped
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

#[test]
fn sub_ifds_are_parsed_beside_the_main_chain() {
    use crate::formats::tiff_family::test_support::{build_tiff_tree, SyntheticIfd, SyntheticTag};

    let ifd = |width| vec![SyntheticTag::long(tags::IMAGE_WIDTH, width)];
    let tmp = build_tiff_tree(&[
        SyntheticIfd {
            tags: ifd(400),
            sub_ifds: vec![ifd(200), ifd(100)],
        },
        SyntheticIfd {
            tags: ifd(50),
            sub_ifds: Vec::new(),
        },
    ]);
    let container = TiffContainer::open(tmp.path()).unwrap();

    assert_eq!(container.top_ifds().len(), 2);
    assert_eq!(container.ifd_count(), 4);
    let [first, second] = container.top_ifds() else {
        panic!("expected two top-level IFDs");
    };
    let widths = container
        .sub_ifds(*first)
        .iter()
        .map(|&child| container.get_u64(child, tags::IMAGE_WIDTH).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(widths, [200, 100]);
    assert!(container.sub_ifds(*second).is_empty());
    assert_eq!(container.get_u64(*second, tags::IMAGE_WIDTH).unwrap(), 50);
}

#[test]
fn sub_ifd_pointing_at_a_parsed_ifd_is_rejected() {
    use crate::formats::tiff_family::test_support::{build_tiff, SyntheticTag};

    // build_tiff writes the only IFD right after the 8-byte header.
    let tmp = build_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 1),
        SyntheticTag::long(tags::SUB_IFDS, 8),
    ]]);
    match TiffContainer::open(tmp.path()).unwrap_err() {
        TiffParseError::Structure(msg) => assert!(msg.contains("already parsed"), "got: {msg}"),
        other => panic!("expected Structure, got: {other:?}"),
    }
}

// ── NDPI offset fixup tests ───────────────────────────────
