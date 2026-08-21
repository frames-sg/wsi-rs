use super::*;

#[test]
fn extreme_positions_do_not_overflow_distance_calculations() {
    let positions = dedup_positions(vec![i64::MAX, i64::MIN]);
    assert_eq!(positions, vec![i64::MIN, i64::MAX]);
    assert_eq!(nearest_position_index(&positions, i64::MAX - 1), 1);
    assert!(median_step(&positions).is_some());
}
