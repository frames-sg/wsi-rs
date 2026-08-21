use super::*;

#[test]
fn decode_empty_data_fails() {
    let result = decode_jpeg(&[], None, 0, 0);
    assert!(result.is_err());
}
