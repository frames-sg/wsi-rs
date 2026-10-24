//! Shared fixtures and helpers for the shim's ABI tests.

// Each integration test compiles this module and uses only part of it.
#![allow(dead_code)]

use std::ffi::{CStr, CString};
use std::path::Path;

use wsi_rs_test_support::corpus::{
    load_manifest, resolve_entry_path, selected_aliases, CorpusEntry,
};

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the shim is a workspace member")
}

/// A file under the repository's `tests/fixtures`.
pub(crate) fn public_fixture(relative: &str) -> CString {
    let path = workspace_root().join("tests/fixtures").join(relative);
    CString::new(path.to_string_lossy().as_bytes()).expect("fixture path has no NUL")
}

/// The raw JPEG 2000 fixture the ABI contract tests open.
pub(crate) fn rgb_nomct_fixture() -> CString {
    public_fixture("jp2k/rgb_nomct.j2k")
}

/// The public parity-corpus slide for `alias`, resolved like the root
/// crate's corpus tests, or `None` when it is absent. Only an explicitly
/// selected alias is required; the whole corpus is not.
pub(crate) fn fixture_path(alias: &str) -> Option<CString> {
    let entry = public_entry(alias);
    if let Some(path) = resolve_entry_path(&entry, workspace_root()) {
        return Some(
            CString::new(path.to_string_lossy().as_bytes()).expect("fixture path has no NUL"),
        );
    }
    let required = selected_aliases().is_some_and(|aliases| aliases.contains(alias));
    assert!(!required, "required parity fixture is missing: {alias}");
    None
}

fn public_entry(alias: &str) -> CorpusEntry {
    let manifest =
        load_manifest(&workspace_root().join("tests/fixtures/parity_corpus.public.toml"))
            .expect("load the public parity manifest");
    manifest
        .slides
        .into_iter()
        .find(|entry| entry.alias == alias)
        .unwrap_or_else(|| panic!("{alias} is not in the public parity manifest"))
}

/// Copy a C string the shim returned.
///
/// # Safety
///
/// `ptr` must be non-null and point to a live NUL-terminated C string for the
/// duration of the copy.
pub(crate) unsafe fn c_string(ptr: *const std::os::raw::c_char) -> String {
    // SAFETY: The caller guarantees a live NUL-terminated string.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn fnv1a_argb(pixels: &[u32]) -> u64 {
    pixels
        .iter()
        .flat_map(|pixel| pixel.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}
