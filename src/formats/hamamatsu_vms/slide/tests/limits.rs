use super::vms_image_count;

#[test]
fn vms_image_count_rejects_oversized_grids() {
    assert_eq!(vms_image_count(2, 3), Some(6));
    assert_eq!(vms_image_count(u32::MAX, u32::MAX), None);
}
