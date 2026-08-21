use super::*;

#[test]
fn exactly_one_accepts_one_and_rejects_other_cardinalities() {
    assert_eq!(exactly_one(vec![7], "test").expect("one item"), 7);
    for values in [Vec::<u8>::new(), vec![1, 2]] {
        assert!(matches!(
            exactly_one(values, "test"),
            Err(WsiError::BackendContract { .. })
        ));
    }
}

#[test]
fn exactly_one_or_else_preserves_contextual_cardinality_errors() {
    assert_eq!(exactly_one_or_else(vec![9], |count| count).unwrap(), 9);
    assert_eq!(exactly_one_or_else(Vec::<u8>::new(), |count| count), Err(0));
    assert_eq!(exactly_one_or_else(vec![1, 2], |count| count), Err(2));
}
