//! Parity-corpus manifest loader.

use std::path::{Path, PathBuf};

#[allow(unused_imports)]
pub(crate) use wsi_rs_test_support::corpus::{
    apply_alias_filter, corpus_cache_dir, format_default_extension, parse_manifest, CorpusEntry,
    CorpusManifest,
};
use wsi_rs_test_support::corpus::{load_manifest, resolve_entry_path as resolve_shared_entry};

pub(crate) fn load_public() -> Result<CorpusManifest, String> {
    let mut manifest = load_public_unfiltered()?;
    apply_alias_filter(
        &mut manifest,
        std::env::var("WSI_RS_PARITY_ALIASES").ok().as_deref(),
    );
    Ok(manifest)
}

pub(crate) fn load_public_unfiltered() -> Result<CorpusManifest, String> {
    load_manifest(&public_manifest_path())
}

pub(crate) fn load_private() -> Result<Option<CorpusManifest>, String> {
    let path = std::env::var_os("WSI_RS_PARITY_PRIVATE_MANIFEST")
        .map(PathBuf::from)
        .unwrap_or_else(private_manifest_path);
    path.is_file().then(|| load_manifest(&path)).transpose()
}

pub(crate) fn resolve_entry_path(entry: &CorpusEntry) -> PathBuf {
    resolve_shared_entry(entry, workspace_root()).unwrap_or_else(|| {
        wsi_rs_test_support::corpus::cache_candidates(entry)
            .into_iter()
            .next()
            .unwrap_or_else(|| corpus_cache_dir().join(&entry.alias))
    })
}

pub(crate) fn find_slide_by_alias(alias: &str) -> Option<PathBuf> {
    load_public()
        .into_iter()
        .chain(load_private().ok().flatten())
        .flat_map(|manifest| manifest.slides)
        .find(|entry| entry.alias == alias)
        .map(|entry| resolve_entry_path(&entry))
        .filter(|path| path.is_file())
}

pub(crate) fn public_manifest_path() -> PathBuf {
    workspace_root().join("tests/fixtures/parity_corpus.public.toml")
}

fn private_manifest_path() -> PathBuf {
    workspace_root().join("tests/fixtures/parity_corpus.private.toml")
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}
