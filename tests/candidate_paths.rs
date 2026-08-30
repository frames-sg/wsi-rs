use std::path::Path;

use wsi_rs::{is_builtin_slide_candidate_path, BUILTIN_SLIDE_CANDIDATE_EXTENSIONS};

const EXPECTED_EXTENSIONS: &[&str] = &[
    "svs", "tif", "tiff", "ndpi", "scn", "bif", "dcm", "czi", "zvi", "mrxs", "vms", "vmu", "vsi",
    "j2k", "j2c", "svcache",
];

#[test]
fn builtin_slide_candidate_extensions_are_the_supported_inventory_set() {
    assert_eq!(BUILTIN_SLIDE_CANDIDATE_EXTENSIONS, EXPECTED_EXTENSIONS);
}

#[test]
fn builtin_slide_candidate_path_matches_extensions_case_insensitively() {
    for extension in EXPECTED_EXTENSIONS {
        assert!(
            is_builtin_slide_candidate_path(Path::new(&format!("slide.{extension}"))),
            "lowercase .{extension}"
        );
        assert!(
            is_builtin_slide_candidate_path(Path::new(&format!(
                "slide.{}",
                extension.to_ascii_uppercase()
            ))),
            "uppercase .{extension}"
        );
    }
}

#[test]
fn builtin_slide_candidate_path_rejects_missing_or_unknown_extensions() {
    for path in ["slide", ".svs", "slide.png", "slide.svs.tmp"] {
        assert!(!is_builtin_slide_candidate_path(Path::new(path)), "{path}");
    }
}
