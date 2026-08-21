use super::*;

#[test]
fn index_budget_rejects_cycles_negative_and_huge_pages() {
    let path = Path::new("slide.mrxs");
    let mut budget = MiraxIndexBudget::default();
    assert_eq!(budget.record_page(path, 100, 2).expect("first page"), 2);
    assert!(budget.record_page(path, 100, 0).is_err());

    let mut budget = MiraxIndexBudget::default();
    assert!(budget.record_page(path, 1, -1).is_err());
    assert!(budget
        .record_page(path, 2, (MAX_MIRAX_RECORDS_PER_PAGE + 1) as i32)
        .is_err());
}
