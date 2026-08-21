use super::zvi_image_len;

#[test]
fn zvi_image_len_rejects_oversized_planes() {
    assert_eq!(zvi_image_len(10, 20, 2).unwrap(), 400);
    assert!(zvi_image_len(u32::MAX, u32::MAX, 4).is_err());
}
