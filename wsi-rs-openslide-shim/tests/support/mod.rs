use std::ffi::CString;
use std::path::PathBuf;

pub(crate) fn fixture_path(alias: &str, extension: &str) -> Option<CString> {
    let cache = std::env::var_os("WSI_RS_PARITY_CORPUS_CACHE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join(".cache")
                    .join("wsi-rs")
                    .join("parity-corpus")
            })
        })?;
    let path = cache.join(format!("{alias}.{extension}"));
    if path.is_file() {
        return Some(
            CString::new(path.to_string_lossy().as_bytes()).expect("fixture path has no NUL"),
        );
    }
    // Only an explicitly selected alias is required; the whole corpus is not.
    let required = wsi_rs_test_support::corpus::selected_aliases()
        .is_some_and(|aliases| aliases.contains(alias));
    assert!(
        !required,
        "required parity fixture is missing: {}",
        path.display()
    );
    None
}

pub(crate) fn fnv1a_argb(pixels: &[u32]) -> u64 {
    pixels
        .iter()
        .flat_map(|pixel| pixel.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}
