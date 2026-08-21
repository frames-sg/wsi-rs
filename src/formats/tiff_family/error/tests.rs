use super::*;

#[test]
fn display_io_error() {
    let err = TiffParseError::Io {
        kind: std::io::ErrorKind::NotFound,
        source: Arc::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        )),
        path: None,
    };
    let s = err.to_string();
    assert!(s.contains("I/O error"), "got: {}", s);
    assert!(s.contains("file not found"), "got: {}", s);
}

#[test]
fn display_invalid_tag() {
    let err = TiffParseError::InvalidTag {
        ifd_offset: 1024,
        tag: 256,
        message: "expected LONG, got ASCII".into(),
    };
    let s = err.to_string();
    assert!(s.contains("1024"), "got: {}", s);
    assert!(s.contains("256"), "got: {}", s);
    assert!(s.contains("expected LONG"), "got: {}", s);
}

#[test]
fn display_bounds() {
    let err = TiffParseError::Bounds {
        offset: 999999,
        len: 4096,
    };
    let s = err.to_string();
    assert!(s.contains("999999"), "got: {}", s);
    assert!(s.contains("4096"), "got: {}", s);
}

#[test]
fn display_structure() {
    let err = TiffParseError::Structure("IFD chain loop detected".into());
    assert!(err.to_string().contains("loop detected"));
}

#[test]
fn display_ifd_not_found() {
    let err = TiffParseError::IfdNotFound(IfdId(8192));
    let s = err.to_string();
    assert!(s.contains("8192"), "got: {}", s);
}

#[test]
fn display_tag_not_found() {
    let err = TiffParseError::TagNotFound {
        ifd_offset: 512,
        tag: 322,
    };
    let s = err.to_string();
    assert!(s.contains("512"), "got: {}", s);
    assert!(s.contains("322"), "got: {}", s);
}

#[test]
fn from_io_error() {
    let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
    let parse_error: TiffParseError = io_err.into();
    match &parse_error {
        TiffParseError::Io { kind, source, .. } => {
            assert_eq!(*kind, std::io::ErrorKind::PermissionDenied);
            assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(source.to_string().contains("access denied"));
        }
        other => panic!("expected Io variant, got: {:?}", other),
    }
}

#[test]
fn into_wsi_error_conversion() {
    let parse_error = TiffParseError::Structure("bad header".into());
    let path = Path::new("/tmp/slide.svs");
    let wsi_err = parse_error.into_wsi_error(path);
    match wsi_err {
        WsiError::Tiff {
            path: p,
            message: m,
        } => {
            assert_eq!(p, PathBuf::from("/tmp/slide.svs"));
            assert!(m.contains("bad header"), "got: {}", m);
        }
        other => panic!("expected Tiff variant, got: {:?}", other),
    }
}

#[test]
fn into_wsi_error_io_routes_to_io_with_path() {
    let parse_error = TiffParseError::Io {
        kind: std::io::ErrorKind::NotFound,
        source: Arc::new(std::io::Error::new(std::io::ErrorKind::NotFound, "gone")),
        path: None,
    };
    let wsi_err = parse_error.into_wsi_error(Path::new("/tmp/test.ndpi"));
    match wsi_err {
        WsiError::IoWithPath { source, path: p } => {
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            assert!(source.to_string().contains("gone"), "got: {}", source);
            assert_eq!(p, PathBuf::from("/tmp/test.ndpi"));
        }
        other => panic!("expected IoWithPath, got: {:?}", other),
    }
}

#[test]
fn ifd_id_equality_and_display() {
    let a = IfdId(100);
    let b = IfdId(100);
    let c = IfdId(200);
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert!(a.to_string().contains("100"));
}
