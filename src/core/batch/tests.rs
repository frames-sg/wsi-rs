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
