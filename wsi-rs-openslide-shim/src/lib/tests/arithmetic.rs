use super::*;

#[test]
fn pixel_lengths_reject_negative_and_overflowing_dimensions() {
    assert_eq!(checked_pixel_len(2, 3), Some(6));
    assert_eq!(checked_pixel_len(-1, 3), None);
    assert_eq!(checked_pixel_len(i64::MAX, i64::MAX), None);
}

#[test]
fn best_level_uses_openslide_floor_boundaries_and_special_values() {
    let levels = [1.0, 4.0];
    for (request, expected) in [
        (f64::NEG_INFINITY, 0),
        (0.0, 0),
        (1.0, 0),
        (3.0, 0),
        (4.0, 1),
        (f64::INFINITY, 1),
        (f64::NAN, 1),
    ] {
        assert_eq!(
            best_level_for_downsample(&levels, request),
            Some(expected),
            "request {request:?}"
        );
    }
    assert_eq!(best_level_for_downsample(&[], 1.0), None);
}
