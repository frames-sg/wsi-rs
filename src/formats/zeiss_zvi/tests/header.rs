use super::*;

fn variant(value_type: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = value_type.to_le_bytes().to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

fn read_variant(value_type: u16, payload: &[u8]) -> String {
    ByteReader::new(&variant(value_type, payload))
        .read_variant()
        .expect("parse synthetic ZVI variant")
}

#[test]
fn zvi_axis_coordinates_are_bounded_before_channel_allocation() {
    assert_eq!(checked_axis(65_535).unwrap(), 65_535);
    assert!(checked_axis(65_536).is_err());
}

#[test]
fn byte_reader_decodes_supported_scalar_string_and_opaque_variants() {
    assert_eq!(read_variant(0, &[]), "");
    assert_eq!(read_variant(2, &(-12_i16).to_le_bytes()), "-12");
    assert_eq!(read_variant(3, &34_i32.to_le_bytes()), "34");
    assert_eq!(read_variant(19, &56_u32.to_le_bytes()), "56");
    assert_eq!(read_variant(4, &1.5_f32.to_le_bytes()), "1.5");
    assert_eq!(read_variant(5, &2.25_f64.to_le_bytes()), "2.25");
    assert_eq!(read_variant(11, &(-1_i16).to_le_bytes()), "true");
    assert_eq!(read_variant(20, &(-78_i64).to_le_bytes()), "-78");
    assert_eq!(read_variant(21, &90_u64.to_be_bytes()), "90");

    let mut utf16 = 6_u32.to_le_bytes().to_vec();
    utf16.extend_from_slice(&[b'Z', 0, b'V', 0, 0, 0]);
    assert_eq!(read_variant(8, &utf16), "ZV");

    let mut ascii = 3_u16.to_le_bytes().to_vec();
    ascii.extend_from_slice(b"tag");
    assert_eq!(read_variant(66, &ascii), "tag");

    assert_eq!(read_variant(9, &[0; 16]), "");
    let mut opaque = 3_u32.to_le_bytes().to_vec();
    opaque.extend_from_slice(&[1, 2, 3]);
    assert_eq!(read_variant(63, &opaque), "");
}

#[test]
fn byte_reader_handles_unknown_and_truncated_variants_without_panicking() {
    let mut unknown = variant(999, &[b'A', 0]);
    unknown.extend_from_slice(&3_u16.to_le_bytes());
    let mut reader = ByteReader::new(&unknown);
    assert!(reader
        .read_variant()
        .expect("lossy unknown ZVI variant")
        .ends_with('A'));
    assert_eq!(reader.position(), 4);

    assert!(ByteReader::new(&[66, 0, 4, 0, b'a'])
        .read_variant()
        .is_err());
    assert_eq!(decode_utf16le_lossy(&[b'A', 0, b'B']), "A");
    assert!(checked_axis(-1).is_err());
    assert!(checked_dimension(-1).is_err());
}
