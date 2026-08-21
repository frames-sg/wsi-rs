use super::*;

#[test]
fn error_display_formats() {
    let err = WsiError::Tiff {
        path: "/tmp/test.svs".into(),
        message: "bad IFD".into(),
    };
    assert!(err.to_string().contains("test.svs"));
    assert!(err.to_string().contains("bad IFD"));
}

#[test]
fn io_error_converts() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let wsi_err: WsiError = io_err.into();
    assert!(matches!(wsi_err, WsiError::Io(_)));
}

#[test]
fn scene_out_of_range_display() {
    let err = WsiError::SceneOutOfRange { index: 2, count: 1 };
    assert!(err.to_string().contains("2"));
    assert!(err.to_string().contains("1"));
}

#[test]
fn series_out_of_range_display() {
    let err = WsiError::SeriesOutOfRange { index: 3, count: 2 };
    assert!(err.to_string().contains("3"));
}

#[test]
fn plane_out_of_range_display() {
    let err = WsiError::PlaneOutOfRange {
        axis: "z".into(),
        value: 5,
        max: 3,
    };
    assert!(err.to_string().contains("z"));
    assert!(err.to_string().contains("5"));
}

#[test]
fn level_out_of_range_display() {
    let err = WsiError::LevelOutOfRange {
        level: 10,
        count: 5,
    };
    assert!(err.to_string().contains("10"));
}

#[test]
fn associated_image_not_found_display() {
    let err = WsiError::AssociatedImageNotFound("label".into());
    assert!(err.to_string().contains("label"));
}

#[test]
fn display_conversion_display() {
    let err = WsiError::DisplayConversion("non-uint8 requires windowing".into());
    assert!(err.to_string().contains("windowing"));
}

#[test]
fn io_with_path_display() {
    let err = WsiError::IoWithPath {
        source: Arc::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        )),
        path: "/tmp/slide.svs".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("/tmp/slide.svs"), "got: {msg}");
    assert!(msg.contains("file not found"), "got: {msg}");
}

#[test]
fn resource_limit_preserves_typed_byte_counts() {
    let err = WsiError::ResourceLimit {
        resource: "compressed DICOM frame",
        requested: 513,
        limit: 512,
    };
    assert!(err.to_string().contains("compressed DICOM frame"));
    assert!(matches!(
        err,
        WsiError::ResourceLimit {
            requested: 513,
            limit: 512,
            ..
        }
    ));
}

#[test]
fn codec_display_includes_codec_and_source() {
    let inner: Box<dyn std::error::Error + Send + Sync> = "boom".into();
    let err = WsiError::Codec {
        codec: "jpeg",
        source: inner,
    };
    let msg = err.to_string();
    assert!(msg.contains("jpeg"), "got: {msg}");
    assert!(msg.contains("boom"), "got: {msg}");
}

#[test]
fn codec_pattern_match_round_trips() {
    let err = WsiError::Codec {
        codec: "j2k",
        source: "decode failed".into(),
    };
    match err {
        WsiError::Codec { codec, source: _ } => assert_eq!(codec, "j2k"),
        other => panic!("expected Codec, got {other:?}"),
    }
}

#[test]
fn unsupported_display() {
    let err = WsiError::Unsupported {
        reason: "device backend unavailable".into(),
    };
    assert!(err.to_string().contains("device backend unavailable"));
}
