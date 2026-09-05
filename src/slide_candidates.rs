use std::path::Path;

/// File extensions recognized by the built-in slide readers.
///
/// Entries are lowercase and do not include the leading dot. Call
/// [`is_builtin_slide_candidate_path`] when matching a path so extension case
/// is handled consistently.
pub const BUILTIN_SLIDE_CANDIDATE_EXTENSIONS: &[&str] = &[
    "svs", "avs", "tif", "tiff", "ndpi", "scn", "bif", "dcm", "czi", "zvi", "mrxs", "vms", "vmu",
    "vsi", "j2k", "j2c", "svcache",
];

/// Returns whether `path` has an extension handled by a built-in slide reader.
pub fn is_builtin_slide_candidate_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            BUILTIN_SLIDE_CANDIDATE_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}
