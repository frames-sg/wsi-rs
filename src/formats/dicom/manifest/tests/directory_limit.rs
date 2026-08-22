use super::{direct_child_files, MAX_DICOM_DIRECTORY_FILES};

#[test]
fn dicom_directory_file_limit_is_bounded() {
    let limit = std::hint::black_box(MAX_DICOM_DIRECTORY_FILES);
    assert!((1_000..=100_000).contains(&limit));
}

#[test]
fn dicom_directory_scan_rejects_more_than_1024_candidates() {
    let directory = tempfile::tempdir().expect("temporary DICOM directory");
    for index in 0..=1_024 {
        std::fs::File::create(directory.path().join(format!("{index:04}.dcm")))
            .expect("create DICOM candidate");
    }

    let error = direct_child_files(directory.path())
        .expect_err("oversized DICOM directory scan must be rejected");
    assert!(
        error.to_string().contains("1024 direct files"),
        "unexpected error: {error}"
    );
}
