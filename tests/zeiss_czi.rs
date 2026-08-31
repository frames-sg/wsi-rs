use std::io::Write;

use wsi_rs::{Slide, WsiError};

#[test]
fn builtin_registry_rejects_czi_as_unsupported() {
    let mut file = tempfile::Builder::new()
        .suffix(".czi")
        .tempfile()
        .expect("create CZI-shaped input");
    file.write_all(b"ZISRAWFILE\0\0\0\0\0\0")
        .expect("write CZI magic");

    let error = Slide::open(file.path()).expect_err("CZI must not open through the 0.7 registry");
    assert!(
        matches!(error, WsiError::UnsupportedFormat(_)),
        "unexpected CZI exclusion error: {error}"
    );
}
