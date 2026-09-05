use super::*;

#[test]
fn positive_integer_values_reject_zero_invalid_and_signed_inputs() {
    assert_eq!(positive_u64_value("1"), Some(1));
    assert_eq!(positive_u64_value(&u64::MAX.to_string()), Some(u64::MAX));
    assert_eq!(positive_u64_value("0"), None);
    assert_eq!(positive_u64_value("-1"), None);
    assert_eq!(positive_u64_value(" 1 "), None);
    assert_eq!(positive_u64_value("invalid"), None);
}
