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

#[test]
fn verified_open_and_match_accept_the_unchanged_file() {
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("slide.bin");
    std::fs::write(&path, b"one").expect("write file");
    let identity = FileIdentity::from_path(&path).expect("identity");

    identity
        .ensure_matches(&path, "unexpected")
        .expect("unchanged path matches");
    let mut file =
        FileIdentity::open_verified(&path, &identity, "unexpected").expect("unchanged file opens");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).expect("read verified file");
    assert_eq!(bytes, b"one");
}

#[test]
fn verified_open_and_match_reject_a_replaced_file_with_the_given_message() {
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("slide.bin");
    std::fs::write(&path, b"one").expect("write file");
    let identity = FileIdentity::from_path(&path).expect("identity");
    std::fs::rename(&path, temp.path().join("original.bin")).expect("detach original");
    std::fs::write(&path, b"replacement").expect("replace file");

    for error in [
        identity
            .ensure_matches(&path, "source was replaced")
            .expect_err("replaced path must not match"),
        FileIdentity::open_verified(&path, &identity, "source was replaced")
            .expect_err("replaced file must not open"),
    ] {
        let WsiError::InvalidSlide {
            path: error_path,
            message,
        } = error
        else {
            panic!("expected InvalidSlide, got {error:?}");
        };
        assert_eq!(error_path, path);
        assert_eq!(message, "source was replaced");
    }
}

#[test]
fn verified_open_of_a_missing_file_reports_the_open_error() {
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("slide.bin");
    std::fs::write(&path, b"one").expect("write file");
    let identity = FileIdentity::from_path(&path).expect("identity");
    std::fs::remove_file(&path).expect("remove file");

    let error = FileIdentity::open_verified(&path, &identity, "unexpected")
        .expect_err("missing file must not open");
    assert!(
        matches!(&error, WsiError::IoWithPath { path: error_path, .. } if *error_path == path),
        "{error:?}"
    );
}
