use std::collections::HashMap;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

use super::super::helpers::*;
use super::super::index::*;
use super::super::*;
use super::fixtures::{encode_jpeg, patterned_pixels, write_bytes, MiraxFixture};
use crate::core::limits::MAX_COMPRESSED_INPUT_BYTES;

fn error<T>(result: Result<T, WsiError>) -> WsiError {
    match result {
        Ok(_) => panic!("operation unexpectedly succeeded"),
        Err(error) => error,
    }
}

#[test]
fn ini_value_parsers_formats_and_key_expansion_are_explicit() {
    let path = Path::new("synthetic.mrxs");
    let mut group = HashMap::new();
    group.insert("text".into(), "value".into());
    group.insert("signed".into(), "-7".into());
    group.insert("unsigned".into(), "42".into());
    group.insert("float".into(), "1.25".into());

    assert_eq!(required_ini_string(path, &group, "text").unwrap(), "value");
    assert_eq!(parse_ini_i32(path, &group, "signed").unwrap(), -7);
    assert_eq!(parse_ini_u32(path, &group, "unsigned").unwrap(), 42);
    assert_eq!(parse_u32_value(path, "direct", "9").unwrap(), 9);
    assert_eq!(parse_ini_f64(path, &group, "float").unwrap(), 1.25);

    for result in [
        required_ini_string(path, &group, "missing").map(|_| ()),
        parse_ini_i32(path, &group, "missing").map(|_| ()),
        parse_ini_u32(path, &group, "missing").map(|_| ()),
        parse_ini_f64(path, &group, "missing").map(|_| ()),
    ] {
        assert!(matches!(error(result), WsiError::InvalidSlide { .. }));
    }
    group.insert("bad".into(), "not-a-number".into());
    assert!(matches!(
        error(parse_ini_i32(path, &group, "bad")),
        WsiError::InvalidSlide { .. }
    ));
    assert!(matches!(
        error(parse_ini_u32(path, &group, "bad")),
        WsiError::InvalidSlide { .. }
    ));
    assert!(matches!(
        error(parse_ini_f64(path, &group, "bad")),
        WsiError::InvalidSlide { .. }
    ));
    assert!(matches!(
        error(parse_u32_value(path, "bad", "-1")),
        WsiError::InvalidSlide { .. }
    ));

    assert_eq!(parse_image_format("JPEG").unwrap(), MiraxImageFormat::Jpeg);
    assert_eq!(parse_image_format("PNG").unwrap(), MiraxImageFormat::Png);
    assert_eq!(
        parse_image_format("BMP24").unwrap(),
        MiraxImageFormat::Bmp24
    );
    assert!(matches!(
        error(parse_image_format("GIF")),
        WsiError::DisplayConversion(message) if message.contains("GIF")
    ));
    assert_eq!(bgr_to_rgb(0x11_22_33), 0x33_22_11);
    assert_eq!(fmt_key("ITEM_%d_VALUE", 3), "ITEM_3_VALUE");
    assert_eq!(fmt_key2("ITEM_%d_%d", 2, 5), "ITEM_2_5");
}

#[test]
fn binary_readers_hash_ranges_and_record_limits_keep_path_context() {
    let mut source = tempfile::NamedTempFile::new().expect("temporary MIRAX binary");
    source.write_all(b"abc").unwrap();
    source.write_all(&42i32.to_le_bytes()).unwrap();
    source.write_all(&(-1i32).to_le_bytes()).unwrap();
    source.flush().unwrap();

    let mut file = File::open(source.path()).unwrap();
    assert_eq!(
        read_exact_string(&mut file, source.path(), 3).unwrap(),
        "abc"
    );
    assert_eq!(read_i32_le(&mut file, source.path()).unwrap(), 42);
    assert!(matches!(
        error(read_u32_le(&mut file, source.path())),
        WsiError::InvalidSlide { message, .. } if message.contains("negative MIRAX pointer")
    ));
    assert!(matches!(
        error(read_i32_le(&mut file, source.path())),
        WsiError::IoWithPath { path, .. } if path == source.path()
    ));

    let record = MiraxRecord {
        path: source.path().to_path_buf(),
        offset: 1,
        len: 3,
    };
    assert_eq!(read_record_bytes(&record).unwrap(), b"bc\x2a");
    assert_eq!(
        read_record_bytes_fields(source.path(), 0, 3).unwrap(),
        b"abc"
    );
    assert!(matches!(
        error(read_record_bytes_fields(source.path(), 8, 8)),
        WsiError::IoWithPath { .. }
    ));
    assert!(matches!(
        error(read_record_bytes_fields(
            source.path(),
            0,
            MAX_COMPRESSED_INPUT_BYTES + 1,
        )),
        WsiError::InvalidSlide { .. }
    ));

    let mut quickhash = Quickhash1::new();
    let mut files = HashMap::new();
    quickhash_file_part_cached(&mut quickhash, &mut files, source.path(), 0, 3).unwrap();
    quickhash_file_part_cached(&mut quickhash, &mut files, source.path(), 3, 4).unwrap();
    assert_eq!(files.len(), 1);
    assert!(quickhash.finish().is_some());
    assert!(matches!(
        error(quickhash_file_part_cached(
            &mut Quickhash1::new(),
            &mut HashMap::new(),
            source.path(),
            u64::MAX,
            2,
        )),
        WsiError::IoWithPath { source, .. }
            if source.kind() == std::io::ErrorKind::UnexpectedEof
    ));

    let missing = source.path().with_file_name("missing-mirax-data.bin");
    assert!(matches!(
        error(read_record_bytes_fields(&missing, 0, 1)),
        WsiError::IoWithPath { path, .. } if path == missing
    ));
    assert!(matches!(
        error(quickhash_file_part_cached(
            &mut Quickhash1::new(),
            &mut HashMap::new(),
            &missing,
            0,
            1,
        )),
        WsiError::IoWithPath { path, .. } if path == missing
    ));

    let rgb = image::RgbImage::from_raw(2, 1, patterned_pixels(2, 1, 4)).unwrap();
    let tile = rgb_image_to_sample_buffer(rgb);
    assert_eq!((tile.width(), tile.height(), tile.channels()), (2, 1, 3));
}

#[test]
fn associated_jpeg_dimension_probe_falls_back_to_the_complete_record() {
    let temp = tempfile::tempdir().unwrap();
    let data_path = temp.path().join("associated.dat");
    let mut jpeg = encode_jpeg(13, 7, 30);
    let mut long_comment = vec![0xff, 0xfe, 0xff, 0xff];
    long_comment.resize(4 + 65_533, 0);
    jpeg.splice(2..2, long_comment);
    write_bytes(&data_path, &jpeg);
    let record = MiraxRecord {
        path: data_path,
        offset: 0,
        len: jpeg.len() as u64,
    };
    let mut files = HashMap::new();

    assert_eq!(
        read_jpeg_dimensions_from_record(Path::new("slide.mrxs"), &mut files, &record).unwrap(),
        (13, 7)
    );
    assert_eq!(files.len(), 1);
    assert_eq!(
        read_jpeg_dimensions_from_record(Path::new("slide.mrxs"), &mut files, &record).unwrap(),
        (13, 7)
    );
}

#[test]
fn position_buffers_activity_and_fallback_grid_cover_sparse_layouts() {
    let path = Path::new("slide.mrxs");
    let mut bytes = Vec::new();
    for (flag, x, y) in [(0u8, 3i32, -4i32), (1, 5, 6)] {
        bytes.push(flag);
        bytes.extend_from_slice(&x.to_le_bytes());
        bytes.extend_from_slice(&y.to_le_bytes());
    }
    assert_eq!(
        read_slide_position_buffer(path, &bytes, 2).unwrap(),
        vec![6, -8, 10, 12]
    );
    assert!(matches!(
        error(read_slide_position_buffer(path, &[0; 8], 1)),
        WsiError::InvalidSlide { .. }
    ));
    let mut invalid_flag = bytes.clone();
    invalid_flag[0] = 2;
    assert!(matches!(
        error(read_slide_position_buffer(path, &invalid_flag, 1)),
        WsiError::InvalidSlide { .. }
    ));

    let params = [
        SlideZoomLevelParams {
            image_concat: 1,
            tile_count_divisor: 1,
            tiles_per_image: 1,
            positions_per_tile: 1,
            tile_advance_x: 16.0,
            tile_advance_y: 16.0,
        },
        SlideZoomLevelParams {
            image_concat: 2,
            tile_count_divisor: 1,
            tiles_per_image: 2,
            positions_per_tile: 1,
            tile_advance_x: 8.0,
            tile_advance_y: 8.0,
        },
    ];
    let positions = [0, 0, 16, 0, 0, 16, 0, 0];
    let mut active = vec![false; 4];
    assert_eq!(
        get_tile_position(&positions, &mut active, &params, 2, 1, 16, 16, 0, 0, 0).unwrap(),
        Some((0, 0))
    );
    assert_eq!(
        get_tile_position(&positions, &mut active, &params, 2, 1, 16, 16, 0, 1, 1).unwrap(),
        None
    );
    assert_eq!(
        get_tile_position(&positions, &mut active, &params, 2, 1, 16, 16, 1, 0, 0).unwrap(),
        Some((0, 0))
    );
    assert_eq!(
        get_tile_position(&[], &mut active, &params, 2, 1, 16, 16, 0, 0, 0).unwrap(),
        None
    );

    let mut empty_index = tempfile::NamedTempFile::new().unwrap();
    let fallback = load_slide_positions(
        path,
        empty_index.as_file_mut(),
        &[],
        0,
        None,
        None,
        2,
        2,
        1,
        1,
        16,
        8,
        2.0,
        1.0,
    )
    .unwrap();
    assert_eq!(fallback, vec![0, 0, 14, 0, 0, 7, 14, 7]);
    assert!(matches!(
        error(load_slide_positions(
            path,
            empty_index.as_file_mut(),
            &[],
            0,
            None,
            None,
            0,
            2,
            1,
            1,
            16,
            8,
            0.0,
            0.0,
        )),
        WsiError::InvalidSlide { .. }
    ));
    assert!(matches!(
        error(load_slide_positions(
            path,
            empty_index.as_file_mut(),
            &[],
            0,
            Some(-1),
            None,
            1,
            1,
            1,
            1,
            16,
            8,
            0.0,
            0.0,
        )),
        WsiError::InvalidSlide { message, .. } if message.contains("negative MIRAX nonhier record")
    ));
}

#[test]
fn fractional_edge_tile_dimensions_stay_within_the_backing_image() {
    let image = Arc::new(MiraxImage {
        id: 0,
        record: MiraxRecord {
            path: PathBuf::from("tile.jpg"),
            offset: 0,
            len: 1,
        },
        format: MiraxImageFormat::Jpeg,
        expected_width: 340,
        expected_height: 256,
    });
    let mut level = MiraxLevelBuilder {
        dimensions: (340, 256),
        downsample: 1.0,
        image_format: MiraxImageFormat::Jpeg,
        raw_image_width: 340,
        raw_image_height: 256,
        tile_width: 42.5,
        tile_height: 32.0,
        tile_advance_x: 42.5,
        tile_advance_y: 32.0,
        tiles: HashMap::new(),
        descriptors: Vec::new(),
        extra_tiles: (0, 0, 0, 0),
    };
    let params = SlideZoomLevelParams {
        image_concat: 8,
        tile_count_divisor: 1,
        tiles_per_image: 8,
        positions_per_tile: 1,
        tile_advance_x: 42.5,
        tile_advance_y: 32.0,
    };

    insert_tile(&mut level, &params, image, 297.5, 0.0, 298, 0, 7, 0);

    assert_eq!(level.tiles[&(7, 0)].dimensions, (42, 32));
}

#[test]
fn index_header_and_nonhierarchical_lookups_validate_identity_and_formats() {
    let fixture = MiraxFixture::complete();
    let ini = parse_mirax_ini(&fixture.slidedat_path).unwrap();
    assert_eq!(
        get_nonhier_name_offset(
            &fixture.path,
            &ini,
            2,
            GROUP_HIERARCHICAL,
            VALUE_VIMSLIDE_POSITION_BUFFER,
        )
        .unwrap(),
        Some(0)
    );
    assert_eq!(
        get_associated_image_nonhier_offset(
            &fixture.path,
            &ini,
            2,
            GROUP_HIERARCHICAL,
            VALUE_SCAN_DATA_LAYER,
            VALUE_SCAN_DATA_LAYER_LABEL,
            KEY_LABEL_IMAGE_TYPE,
        )
        .unwrap(),
        Some(2)
    );
    assert_eq!(
        get_nonhier_val_offset(
            &fixture.path,
            &ini,
            2,
            GROUP_HIERARCHICAL,
            "absent",
            "absent",
        )
        .unwrap(),
        None
    );

    let mut index = File::open(&fixture.index_path).unwrap();
    verify_index_header(&fixture.path, &mut index, "SYNTHETIC").unwrap();
    index.seek(SeekFrom::Start(0)).unwrap();
    assert!(matches!(
        error(verify_index_header(&fixture.path, &mut index, "MISMATCH!")),
        WsiError::InvalidSlide { message, .. } if message.contains("identifier")
    ));
    let mut bytes = fixture.read_index();
    bytes[0] = b'9';
    fixture.write_index(&bytes);
    let mut index = File::open(&fixture.index_path).unwrap();
    assert!(matches!(
        error(verify_index_header(&fixture.path, &mut index, "SYNTHETIC")),
        WsiError::InvalidSlide { message, .. } if message.contains("version")
    ));
}
