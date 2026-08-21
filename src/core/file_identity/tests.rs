use super::*;

#[test]
fn identity_changes_when_file_length_changes() {
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("slide.bin");
    std::fs::write(&path, b"one").expect("write initial file");
    let first = FileIdentity::from_path(&path).expect("first identity");

    std::fs::write(&path, b"replacement").expect("replace file");
    let second = FileIdentity::from_path(&path).expect("second identity");

    assert_ne!(first, second);
}

#[test]
fn missing_path_is_an_explicit_error() {
    let temp = tempfile::tempdir().expect("temp directory");
    let error =
        FileIdentity::from_path(&temp.path().join("missing")).expect_err("missing path must fail");
    assert!(matches!(error, WsiError::IoWithPath { .. }));
}
