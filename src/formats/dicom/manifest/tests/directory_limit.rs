use super::MAX_DICOM_DIRECTORY_FILES;

#[test]
fn dicom_directory_file_limit_is_bounded() {
    let limit = std::hint::black_box(MAX_DICOM_DIRECTORY_FILES);
    assert!((1_000..=100_000).contains(&limit));
}
