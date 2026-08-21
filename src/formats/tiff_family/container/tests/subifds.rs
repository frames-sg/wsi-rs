use super::*;

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
