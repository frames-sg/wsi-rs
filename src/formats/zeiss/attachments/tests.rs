use super::*;

#[test]
fn embedded_czi_accepts_jpeg_and_jpegxr_and_rejects_other_compression() {
    assert!(ensure_supported_embedded_czi([
        CziCompressionMode::UnCompressed,
        CziCompressionMode::Jpg,
        CziCompressionMode::JpgXr,
    ])
    .is_ok());
    let error = ensure_supported_embedded_czi([CziCompressionMode::Zstd0])
        .expect_err("unsupported embedded CZI compression must not reach dependency decode");
    assert!(error.to_string().contains("associated-image compression"));
}

#[test]
fn embedded_czi_rejects_oversized_plane_before_dependency_allocation() {
    assert!(ensure_embedded_czi_plane_budget((512, 512), 4).is_ok());
    let error = ensure_embedded_czi_plane_budget((32_768, 32_768), 1)
        .expect_err("oversized embedded plane must be rejected before allocation");
    assert!(error.to_string().contains("embedded CZI plane"));
}

#[test]
fn temporary_czi_blob_is_removed_after_success_and_failure() {
    let directory = tempfile::tempdir().expect("temporary attachment directory");
    for should_fail in [false, true] {
        let mut temporary_path = None;
        let result = with_temporary_czi_blob_in(directory.path(), b"embedded-czi", |path| {
            temporary_path = Some(path.to_path_buf());
            assert_eq!(
                std::fs::read(path).expect("read open temporary blob"),
                b"embedded-czi"
            );
            if should_fail {
                Err(WsiError::DisplayConversion("injected parse failure".into()))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.is_err(), should_fail);
        assert!(
            !temporary_path.expect("closure observed path").exists(),
            "temporary CZI attachment must be removed by Drop"
        );
    }
}

#[cfg(unix)]
#[test]
fn precreated_symlink_cannot_redirect_temporary_attachment_write() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary attachment directory");
    let outside = directory.path().join("outside");
    std::fs::write(&outside, b"preserve-me").expect("write outside sentinel");
    let predictable = directory.path().join("wsi-rs-zeiss-attachment.czi");
    symlink(&outside, &predictable).expect("create hostile symlink");

    let mut actual = None;
    with_temporary_czi_blob_in(directory.path(), b"embedded-czi", |path| {
        actual = Some(path.to_path_buf());
        assert_ne!(path, predictable);
        Ok(())
    })
    .expect("exclusive temporary attachment write");

    assert_eq!(
        std::fs::read(&outside).expect("outside sentinel"),
        b"preserve-me"
    );
    assert!(!actual.expect("temporary path").exists());
    assert!(predictable.is_symlink());
}

#[test]
fn attachment_prefix_read_reports_offset_open_and_truncation_failures() {
    fn attachment(file_position: u64) -> czi_rs::AttachmentInfo {
        czi_rs::AttachmentInfo {
            index: 0,
            file_position,
            file_part: 0,
            content_guid: "00000000-0000-0000-0000-000000000000".into(),
            content_file_type: "JPG".into(),
            name: "Label".into(),
            data_size: 1,
        }
    }

    let overflow = read_attachment_prefix(Path::new("unused.czi"), &attachment(u64::MAX), 1)
        .expect_err("overflowing attachment offset");
    assert!(overflow.to_string().contains("offset overflow"));

    let directory = tempfile::tempdir().expect("attachment prefix directory");
    let missing = directory.path().join("missing.czi");
    assert!(matches!(
        read_attachment_prefix(&missing, &attachment(0), 1),
        Err(WsiError::IoWithPath { path, .. }) if path == missing
    ));

    assert!(matches!(
        read_attachment_prefix(directory.path(), &attachment(0), 1),
        Err(WsiError::IoWithPath { path, .. }) if path == directory.path()
    ));

    let truncated = directory.path().join("truncated.czi");
    std::fs::write(&truncated, []).expect("write truncated attachment fixture");
    assert!(matches!(
        read_attachment_prefix(&truncated, &attachment(0), 1),
        Err(WsiError::IoWithPath { path, .. }) if path == truncated
    ));
}
