use std::ffi::CString;
use std::path::PathBuf;

pub(crate) fn fixture_path(alias: &str, extension: &str) -> Option<CString> {
    let cache = std::env::var_os("WSI_RS_PARITY_CORPUS_CACHE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join(".cache")
                    .join("slideviewer")
                    .join("parity-corpus")
            })
        })?;
    let path = cache.join(format!("{alias}.{extension}"));
    if path.is_file() {
        return Some(
            CString::new(path.to_string_lossy().as_bytes()).expect("fixture path has no NUL"),
        );
    }
    let required = std::env::var("WSI_RS_PARITY_ALIASES").is_ok_and(|aliases| {
        aliases
            .split(',')
            .map(str::trim)
            .any(|candidate| candidate == alias)
    });
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
