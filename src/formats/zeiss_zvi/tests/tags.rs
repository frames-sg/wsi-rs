use super::*;

#[test]
fn tag_parser_rejects_excessive_declared_entry_count() {
    let mut data = vec![0; 12];
    data[8..12].copy_from_slice(&16_385_i32.to_le_bytes());
    assert!(parse_zvi_tags(&data).is_err());
}

#[test]
fn tag_accessors_trim_convert_and_decode_rgb_values() {
    let tags = HashMap::from([
        (1, "  name  ".to_string()),
        (2, "12.6".to_string()),
        (3, "0.125".to_string()),
        (4, "1122867".to_string()),
        (5, "not numeric".to_string()),
        (6, "   ".to_string()),
    ]);
    assert_eq!(tag_string(&tags, 1).as_deref(), Some("name"));
    assert_eq!(tag_u32(&tags, 2), Some(13));
    assert_eq!(tag_f64(&tags, 3), Some(0.125));
    assert_eq!(tag_color(&tags, 4), Some([0x11, 0x22, 0x33]));
    assert_eq!(tag_u32(&tags, 5), None);
    assert_eq!(tag_string(&tags, 6), None);
    assert_eq!(tag_string(&tags, 99), None);
}

#[test]
fn tag_parser_tolerates_negative_counts_and_incomplete_trailing_entries() {
    let mut negative = vec![0; 12];
    negative[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
    assert!(parse_zvi_tags(&negative).unwrap().is_empty());

    let mut truncated = vec![0; 12];
    truncated[8..12].copy_from_slice(&1_i32.to_le_bytes());
    truncated.extend_from_slice(&66_u16.to_le_bytes());
    truncated.extend_from_slice(&1_u16.to_le_bytes());
    truncated.push(b'x');
    truncated.extend_from_slice(&[0; 2]);
    assert!(parse_zvi_tags(&truncated).unwrap().is_empty());
}
