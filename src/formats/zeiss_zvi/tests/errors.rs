use super::super::*;
use super::fixtures::*;

fn open_error(fixture: &ZviFixture) -> WsiError {
    match ZeissZviBackend.open(&fixture.path) {
        Ok(_) => panic!("malformed synthetic ZVI unexpectedly opened"),
        Err(error) => error,
    }
}

fn invalid_message(error: WsiError) -> String {
    match error {
        WsiError::InvalidSlide { message, .. } => message,
        other => panic!("expected InvalidSlide, got {other:?}"),
    }
}

#[test]
fn open_rejects_missing_items_and_truncated_headers() {
    let no_items = ZviFixture::whole_u8();
    for index in 0..3 {
        no_items.remove_stream(&format!("/Image/Item({index})/Contents"));
    }
    assert!(invalid_message(open_error(&no_items)).contains("no image item streams"));

    let truncated = ZviFixture::whole_u8();
    truncated.rewrite_stream("/Image/Item(0)/Contents", &[0; 20]);
    assert!(open_error(&truncated)
        .to_string()
        .contains("unexpected end of ZVI metadata stream"));
}

#[test]
fn open_rejects_bad_dimensions_axes_coordinate_blocks_and_sample_depths() {
    let cases = [
        (0, 2, 0, 0, 0, "zero ZVI dimension"),
        (2, 2, -1, 0, 0, "negative ZVI axis coordinate"),
        (2, 2, 65_536, 0, 0, "exceeds the supported maximum"),
    ];
    for (width, height, z, c, t, expected) in cases {
        let fixture = ZviFixture::whole_u8();
        let bytes = header_bytes(
            &PlaneSpec {
                width,
                height,
                bytes_per_sample: 1,
                z: z as u32,
                c: c as u32,
                t: t as u32,
                tile_index: 0,
                encoding: PlaneEncoding::Raw(Vec::new()),
                tags: Vec::new(),
            },
            2,
        );
        fixture.rewrite_stream("/Image/Item(0)/Contents", &bytes);
        assert!(open_error(&fixture).to_string().contains(expected));
    }

    let short_coordinates = ZviFixture::whole_u8();
    let mut bytes = header_bytes(&PlaneSpec::raw_u8(2, 2, Vec::new()), 2);
    bytes[24..28].copy_from_slice(&27_i32.to_le_bytes());
    short_coordinates.rewrite_stream("/Image/Item(0)/Contents", &bytes);
    assert!(open_error(&short_coordinates)
        .to_string()
        .contains("coordinate block is too short"));

    let mixed = ZviFixture::whole_u8();
    let raw = vec![0; 8 * 4 * 2];
    mixed.rewrite_stream(
        "/Image/Item(0)/Contents",
        &plane_stream(&PlaneSpec {
            width: 8,
            height: 4,
            bytes_per_sample: 2,
            z: 0,
            c: 0,
            t: 0,
            tile_index: 0,
            encoding: PlaneEncoding::Raw(raw),
            tags: Vec::new(),
        }),
    );
    assert!(invalid_message(open_error(&mixed)).contains("mixed ZVI sample byte depths"));
}

#[test]
fn open_validates_raw_lengths_and_mosaic_scaling() {
    let short_raw = ZviFixture::whole_u8();
    let mut stream = plane_stream(&PlaneSpec::raw_u8(8, 4, vec![0; 32]));
    stream.pop();
    short_raw.rewrite_stream("/Image/Item(0)/Contents", &stream);
    assert!(invalid_message(open_error(&short_raw)).contains("raw payload has 31 bytes"));

    let trailing_raw = ZviFixture::whole_u8();
    let mut stream = plane_stream(&PlaneSpec::raw_u8(8, 4, vec![0; 32]));
    stream.push(0);
    trailing_raw.rewrite_stream("/Image/Item(0)/Contents", &stream);
    assert!(invalid_message(open_error(&trailing_raw)).contains("raw payload has 33 bytes"));

    let mosaic = ZviFixture::mosaic();
    mosaic.rewrite_stream(
        "/Image/Tags/Contents",
        &tag_stream(&[(515, "512".into()), (516, "2".into())]),
    );
    assert!(invalid_message(open_error(&mosaic)).contains("missing global pixel scaling"));
}

#[test]
fn corrupt_compressed_payloads_and_thumbnail_report_decode_errors() {
    let corrupt_zlib = ZviFixture::whole_u8();
    let spec = PlaneSpec {
        c: 1,
        encoding: PlaneEncoding::Zlib(Vec::new()),
        ..PlaneSpec::raw_u8(8, 4, Vec::new())
    };
    let mut stream = plane_stream(&spec);
    stream.truncate(105);
    corrupt_zlib.rewrite_stream("/Image/Item(1)/Contents", &stream);
    let reader = ZeissZviBackend
        .open(&corrupt_zlib.path)
        .expect("open corrupt payload metadata");
    assert!(reader
        .read_tile_cpu(&TileRequest::new(0, 0, 0, 0, 0).with_plane(PlaneSelection::new(0, 1, 0)))
        .is_err());

    let corrupt_jpeg = ZviFixture::whole_u8();
    let mut header = header_bytes(
        &PlaneSpec {
            c: 2,
            encoding: PlaneEncoding::Jpeg(Vec::new()),
            ..PlaneSpec::raw_u8(8, 4, Vec::new())
        },
        1,
    );
    header.extend_from_slice(b"not-jpeg");
    corrupt_jpeg.rewrite_stream("/Image/Item(2)/Contents", &header);
    let reader = ZeissZviBackend
        .open(&corrupt_jpeg.path)
        .expect("open corrupt JPEG metadata");
    let jpeg_error = match reader
        .read_tile_cpu(&TileRequest::new(0, 0, 0, 0, 0).with_plane(PlaneSelection::new(0, 2, 0)))
    {
        Ok(_) => panic!("corrupt synthetic JPEG unexpectedly decoded"),
        Err(error) => error,
    };
    assert!(
        jpeg_error.to_string().to_ascii_lowercase().contains("jpeg"),
        "unexpected corrupt JPEG error: {jpeg_error}"
    );

    let corrupt_thumbnail = ZviFixture::whole_u8();
    corrupt_thumbnail.rewrite_stream("/Thumbnail", b"prefixBMnot-a-bitmap");
    assert!(matches!(
        open_error(&corrupt_thumbnail),
        WsiError::DisplayConversion(_)
    ));

    let markerless_thumbnail = ZviFixture::whole_u8();
    markerless_thumbnail.rewrite_stream("/Thumbnail", b"no bitmap marker");
    let reader = ZeissZviBackend
        .open(&markerless_thumbnail.path)
        .expect("ignore markerless thumbnail");
    assert!(reader.dataset().associated_images.is_empty());
}

#[test]
fn reader_reports_each_index_and_tile_boundary() {
    let fixture = ZviFixture::whole_u8();
    let reader = ZeissZviBackend
        .open(&fixture.path)
        .expect("open synthetic ZVI");

    assert!(matches!(
        reader.read_tile_cpu(&TileRequest::new(1, 0, 0, 0, 0)),
        Err(WsiError::SceneOutOfRange { index: 1, count: 1 })
    ));
    assert!(matches!(
        reader.read_tile_cpu(&TileRequest::new(0, 1, 0, 0, 0)),
        Err(WsiError::SeriesOutOfRange { index: 1, count: 1 })
    ));
    assert!(matches!(
        reader.read_tile_cpu(&TileRequest::new(0, 0, 1, 0, 0)),
        Err(WsiError::LevelOutOfRange { level: 1, count: 1 })
    ));

    for request in [
        TileRequest::new(0, 0, 0, -1, 0),
        TileRequest::new(0, 0, 0, 1, 0),
        TileRequest::new(0, 0, 0, 0, -1),
        TileRequest::new(0, 0, 0, 0, 1),
    ] {
        assert!(reader.read_tile_cpu(&request).is_err());
    }
    for (plane, expected_axis, value, max) in [
        (PlaneSelection::new(1, 0, 0), "z", 1, 0),
        (PlaneSelection::new(0, 3, 0), "c", 3, 2),
        (PlaneSelection::new(0, 0, 1), "t", 1, 0),
    ] {
        assert!(matches!(
            reader.read_tile_cpu(&TileRequest::new(0, 0, 0, 0, 0).with_plane(plane)),
            Err(WsiError::PlaneOutOfRange { axis, value: actual, max: actual_max })
                if axis == expected_axis && actual == value && actual_max == max
        ));
    }

    let mosaic = ZviFixture::mosaic();
    let reader = ZeissZviBackend.open(&mosaic.path).expect("open mosaic ZVI");
    assert!(matches!(
        reader.read_tile_cpu(&TileRequest::new(0, 0, 0, 2, 0)),
        Err(WsiError::TileRead { reason, .. }) if reason.contains("mosaic tile not found")
    ));
}
