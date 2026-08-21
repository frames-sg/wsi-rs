use super::irregular_extra_tiles;

#[test]
fn irregular_extra_tiles_reports_each_side_independently() {
    assert_eq!(
        irregular_extra_tiles(-12.0, -7.0, 10.0, 5.0, 14.0, 9.0),
        (0, 2, 0, 2)
    );
    assert_eq!(
        irregular_extra_tiles(3.0, 2.0, 10.0, 5.0, 14.0, 9.0),
        (2, 0, 1, 0)
    );
    assert_eq!(
        irregular_extra_tiles(0.0, 0.0, 10.0, 5.0, 10.0, 5.0),
        (0, 0, 0, 0)
    );
}
