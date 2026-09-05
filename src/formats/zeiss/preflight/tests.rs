use super::*;
use std::io::Cursor;

const FILE_HEADER_BYTES: usize = 32 + 512;
const DIRECTORY_OFFSET: usize = FILE_HEADER_BYTES;

#[test]
fn directory_coordinate_bounds_preserve_signed_origins_and_checked_extents() {
    use super::directory::checked_dimension_bounds;

    assert_eq!(checked_dimension_bounds(-12, 16).unwrap(), (-12, 4));
    assert_eq!(checked_dimension_bounds(12, 0).unwrap(), (12, 12));
    assert!(checked_dimension_bounds(0, -1).is_err());
    assert!(checked_dimension_bounds(i32::MAX, 1).is_err());
}

#[test]
fn directory_entries_validate_geometry_and_declared_record_boundaries() {
    use super::directory::validate_entries;

    let mut record = vec![0_u8; 32];
    record[..2].copy_from_slice(b"DV");
    write_u32(&mut record, 28, 3);
    for (code, start, size) in [
        (b" X \0", -4_i32, 16_i32),
        (b"Y\0\0\0", 8, 16),
        (b"S\0\0\0", 0, 1),
    ] {
        record.extend_from_slice(code);
        record.extend_from_slice(&start.to_le_bytes());
        record.extend_from_slice(&size.to_le_bytes());
        record.extend_from_slice(&[0; 8]);
    }
    let validate = |bytes: &[u8], declared: u64| {
        validate_entries(&mut Cursor::new(bytes), bytes.len() as u64, 0, declared, 1)
    };
    validate(&record, record.len() as u64).expect("signed origins and trimmed axis names");
    assert!(validate(&record, 31).is_err());
    assert!(validate(&record, 51).is_err());
    let mut unsupported = record.clone();
    unsupported[..2].copy_from_slice(b"DE");
    assert!(validate(&unsupported, unsupported.len() as u64).is_err());
    let mut dimensions = record.clone();
    write_u32(&mut dimensions, 28, 1025);
    assert!(validate(&dimensions, dimensions.len() as u64).is_err());

    let mut combined = record.clone();
    let mut distant = record.clone();
    distant[36..40].copy_from_slice(&i32::MIN.to_le_bytes());
    combined.extend_from_slice(&distant);
    assert!(validate_entries(
        &mut Cursor::new(&combined),
        combined.len() as u64,
        0,
        combined.len() as u64,
        2,
    )
    .is_err());
    assert!(validate_entries(
        &mut Cursor::new(&record),
        record.len() as u64,
        u64::MAX,
        1,
        1
    )
    .is_err());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn base_czi(metadata_offset: u64) -> Vec<u8> {
    let mut bytes = vec![0; DIRECTORY_OFFSET + 32 + 128];
    bytes[..16].copy_from_slice(b"ZISRAWFILE\0\0\0\0\0\0");
    write_u64(&mut bytes, 16, 512);
    write_u64(&mut bytes, 24, 512);
    write_u64(&mut bytes, 32 + 52, DIRECTORY_OFFSET as u64);
    write_u64(&mut bytes, 32 + 60, metadata_offset);
    bytes[DIRECTORY_OFFSET..DIRECTORY_OFFSET + 16].copy_from_slice(b"ZISRAWDIRECTORY\0");
    write_u64(&mut bytes, DIRECTORY_OFFSET + 16, 128);
    write_u64(&mut bytes, DIRECTORY_OFFSET + 24, 128);
    bytes
}

#[test]
fn czi_preflight_rejects_excessive_directory_entry_count() {
    let mut bytes = base_czi(0);
    write_u32(&mut bytes, DIRECTORY_OFFSET + 32, (1_000_000_u32) + 1);
    let mut cursor = Cursor::new(bytes.clone());
    let error = preflight_czi_reader(&mut cursor, bytes.len() as u64)
        .expect_err("excessive CZI directory must be rejected");
    assert!(error.to_string().contains("subblock count"), "{error}");
}

#[test]
fn czi_preflight_rejects_oversized_metadata_declaration() {
    let metadata_offset = (DIRECTORY_OFFSET + 32 + 128) as u64;
    let mut bytes = base_czi(metadata_offset);
    bytes.resize(metadata_offset as usize + 32 + 256, 0);
    let offset = metadata_offset as usize;
    bytes[offset..offset + 16].copy_from_slice(b"ZISRAWMETADATA\0\0");
    write_u64(&mut bytes, offset + 16, 256);
    write_u64(&mut bytes, offset + 24, 256);
    write_u32(&mut bytes, offset + 32, (32 * 1024 * 1024) + 1);

    let mut cursor = Cursor::new(bytes.clone());
    let error = preflight_czi_reader(&mut cursor, bytes.len() as u64)
        .expect_err("oversized CZI metadata must be rejected");
    assert!(error.to_string().contains("metadata"), "{error}");
}

#[test]
fn czi_subblock_preflight_rejects_oversized_payload_declaration() {
    let mut bytes = vec![0; 32 + 256];
    bytes[..16].copy_from_slice(b"ZISRAWSUBBLOCK\0\0");
    write_u64(&mut bytes, 16, 256);
    write_u64(&mut bytes, 24, 256);
    write_u64(
        &mut bytes,
        32 + 8,
        crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 1,
    );
    bytes[32 + 16..32 + 18].copy_from_slice(b"DV");

    let mut cursor = Cursor::new(bytes.clone());
    let error = preflight_czi_subblock_reader(&mut cursor, bytes.len() as u64, 0)
        .expect_err("oversized CZI subblock must be rejected");
    assert!(error.to_string().contains("subblock data"), "{error}");
}

#[test]
fn scalar_readers_and_checked_offsets_reject_truncation_and_overflow() {
    assert!(read_u32(&[0; 3], 0, "u32 boundary")
        .expect_err("truncated u32")
        .to_string()
        .contains("truncated"));
    assert!(read_u64(&[0; 7], 0, "u64 boundary")
        .expect_err("truncated u64")
        .to_string()
        .contains("truncated"));
    assert!(checked_add(u64::MAX, 1, "boundary")
        .expect_err("overflowing offset")
        .to_string()
        .contains("overflows"));
}

#[test]
fn segment_header_handles_zero_used_size_and_rejects_bad_ranges() {
    let mut valid = vec![0; 32 + 512];
    valid[..16].copy_from_slice(FILE_MAGIC);
    write_u64(&mut valid, 16, 512);
    let mut cursor = Cursor::new(valid.clone());
    assert_eq!(
        read_segment_header(&mut cursor, valid.len() as u64, 0, FILE_MAGIC, 512, "file")
            .expect("allocated size fallback"),
        512
    );

    let mut bad_magic = valid.clone();
    bad_magic[0] = b'X';
    let mut cursor = Cursor::new(bad_magic.clone());
    assert!(read_segment_header(
        &mut cursor,
        bad_magic.len() as u64,
        0,
        FILE_MAGIC,
        512,
        "file"
    )
    .expect_err("bad segment magic")
    .to_string()
    .contains("magic"));

    let mut too_small = valid.clone();
    write_u64(&mut too_small, 24, 511);
    let mut cursor = Cursor::new(too_small.clone());
    assert!(read_segment_header(
        &mut cursor,
        too_small.len() as u64,
        0,
        FILE_MAGIC,
        512,
        "file"
    )
    .expect_err("undersized segment")
    .to_string()
    .contains("smaller"));

    let mut beyond_end = valid;
    write_u64(&mut beyond_end, 24, 513);
    let mut cursor = Cursor::new(beyond_end.clone());
    assert!(read_segment_header(
        &mut cursor,
        beyond_end.len() as u64,
        0,
        FILE_MAGIC,
        512,
        "file"
    )
    .expect_err("segment beyond file")
    .to_string()
    .contains("beyond file length"));

    let mut cursor = Cursor::new(vec![0; 8]);
    assert!(read_exact_at(&mut cursor, 8, u64::MAX, 2, "overflow")
        .expect_err("read range overflow")
        .to_string()
        .contains("overflows"));
}

#[test]
fn attachment_directory_preflight_handles_empty_negative_and_short_payloads() {
    let attachment_offset = (DIRECTORY_OFFSET + 32 + 128) as u64;
    let mut bytes = base_czi(0);
    write_u64(&mut bytes, 32 + 72, attachment_offset);
    bytes.resize(attachment_offset as usize + 32 + 256, 0);
    let offset = attachment_offset as usize;
    bytes[offset..offset + 16].copy_from_slice(ATTACHMENT_DIRECTORY_MAGIC);
    write_u64(&mut bytes, offset + 16, 256);
    write_u64(&mut bytes, offset + 24, 256);

    for count in [0_u32, u32::MAX] {
        write_u32(&mut bytes, offset + 32, count);
        let mut cursor = Cursor::new(bytes.clone());
        preflight_czi_reader(&mut cursor, bytes.len() as u64)
            .expect("nonpositive attachment count is empty");
    }

    write_u32(&mut bytes, offset + 32, 1);
    let mut cursor = Cursor::new(bytes.clone());
    assert!(preflight_czi_reader(&mut cursor, bytes.len() as u64)
        .expect_err("missing attachment directory entry")
        .to_string()
        .contains("payload"));

    write_u32(&mut bytes, offset + 32, (MAX_CZI_ATTACHMENTS + 1) as u32);
    let mut cursor = Cursor::new(bytes.clone());
    assert!(preflight_czi_reader(&mut cursor, bytes.len() as u64)
        .expect_err("excessive attachment count")
        .to_string()
        .contains("attachment count"));
}

#[test]
fn subblock_preflight_rejects_each_variable_section_boundary() {
    fn base_subblock() -> Vec<u8> {
        let mut bytes = vec![0; 32 + 256];
        bytes[..16].copy_from_slice(SUBBLOCK_MAGIC);
        write_u64(&mut bytes, 16, 256);
        write_u64(&mut bytes, 24, 256);
        bytes[32 + 16..32 + 18].copy_from_slice(b"DV");
        bytes
    }

    let mut metadata = base_subblock();
    write_u32(&mut metadata, 32, (SUBBLOCK_METADATA_BYTES + 1) as u32);
    let mut cursor = Cursor::new(metadata.clone());
    assert!(
        preflight_czi_subblock_reader(&mut cursor, metadata.len() as u64, 0)
            .expect_err("oversized subblock metadata")
            .to_string()
            .contains("metadata")
    );

    let mut attachment = base_subblock();
    write_u32(
        &mut attachment,
        32 + 4,
        (SUBBLOCK_ATTACHMENT_BYTES + 1) as u32,
    );
    let mut cursor = Cursor::new(attachment.clone());
    assert!(
        preflight_czi_subblock_reader(&mut cursor, attachment.len() as u64, 0)
            .expect_err("oversized subblock attachment")
            .to_string()
            .contains("attachment")
    );

    let mut dimensions = base_subblock();
    write_u32(&mut dimensions, 32 + 44, 1_025);
    let mut cursor = Cursor::new(dimensions.clone());
    assert!(
        preflight_czi_subblock_reader(&mut cursor, dimensions.len() as u64, 0)
            .expect_err("excessive subblock dimensions")
            .to_string()
            .contains("dimension count")
    );

    let mut sections = base_subblock();
    write_u64(&mut sections, 32 + 8, 1);
    let mut cursor = Cursor::new(sections.clone());
    assert!(
        preflight_czi_subblock_reader(&mut cursor, sections.len() as u64, 0)
            .expect_err("subblock sections exceed segment")
            .to_string()
            .contains("exceeding")
    );

    let mut schema = base_subblock();
    schema[32 + 16..32 + 18].copy_from_slice(b"DE");
    let mut cursor = Cursor::new(schema.clone());
    assert!(
        preflight_czi_subblock_reader(&mut cursor, schema.len() as u64, 0)
            .expect_err("unsupported subblock schema")
            .to_string()
            .contains("schema")
    );
}

#[test]
fn file_preflight_wrappers_preserve_paths_for_io_and_parse_errors() {
    let directory = tempfile::tempdir().expect("preflight directory");
    let missing = directory.path().join("missing.czi");
    assert!(matches!(
        preflight_czi_file(&missing),
        Err(WsiError::IoWithPath { path, .. }) if path == missing
    ));
    assert!(matches!(
        preflight_czi_subblock(&missing, 0),
        Err(WsiError::IoWithPath { path, .. }) if path == missing
    ));

    let mut valid_subblock = vec![0; 32 + 256];
    valid_subblock[..16].copy_from_slice(SUBBLOCK_MAGIC);
    write_u64(&mut valid_subblock, 16, 256);
    write_u64(&mut valid_subblock, 24, 256);
    valid_subblock[32 + 16..32 + 18].copy_from_slice(b"DV");
    let path = directory.path().join("subblock.czi");
    std::fs::write(&path, valid_subblock).expect("write valid subblock fixture");
    preflight_czi_subblock(&path, 0).expect("valid subblock wrapper");
    assert!(matches!(
        preflight_czi_file(&path),
        Err(WsiError::InvalidSlide { path: error_path, .. }) if error_path == path
    ));
}
