use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use wsi_rs_test_support::corpus::{
    self, CorpusEntry as ManifestSlide, CorpusManifest as ParityManifest,
};

use super::worker::workspace_root;

const MANIFEST_ENV: &str = "WSI_RS_PERF_MANIFEST";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SlideSpec {
    pub(super) path: PathBuf,
    pub(super) alias: String,
    pub(super) format: String,
    pub(super) benchmark_group: String,
    pub(super) manifest_sha256: Option<String>,
}

impl SlideSpec {
    pub(super) fn custom(path: PathBuf) -> Self {
        let alias = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("custom")
            .to_string();
        Self {
            path,
            alias,
            format: "custom".into(),
            benchmark_group: "custom".into(),
            manifest_sha256: None,
        }
    }
}

pub(super) fn load_manifest() -> Result<ParityManifest, String> {
    let path = std::env::var_os(MANIFEST_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(default_manifest_path);
    corpus::load_manifest(&path).map_err(|error| {
        error.replacen(
            "failed to read manifest",
            "failed to read performance manifest",
            1,
        )
    })
}

#[cfg(test)]
fn parse_manifest(text: &str) -> Result<ParityManifest, String> {
    corpus::parse_manifest(text)
}

pub(super) fn resolve_manifest_slides(
    manifest: &ParityManifest,
    selectors: &[String],
    require_openslide: bool,
    allow_custom_paths: bool,
) -> Result<Vec<SlideSpec>, String> {
    if selectors.is_empty() {
        let selected = manifest
            .slides
            .iter()
            .filter(|entry| {
                !entry.must_decode.is_empty() && (!require_openslide || entry.openslide_required)
            })
            .map(resolve_entry)
            .collect::<Result<Vec<_>, _>>()?;
        if selected.is_empty() {
            return Err("performance manifest selected no slides".into());
        }
        return Ok(selected);
    }

    let mut selected = Vec::with_capacity(selectors.len());
    let mut seen = BTreeSet::new();
    for selector in selectors {
        let slide = if let Some(entry) = manifest
            .slides
            .iter()
            .find(|entry| entry.alias == *selector)
        {
            require_openslide_compatibility(entry, require_openslide)?;
            resolve_entry(entry)?
        } else {
            let path = PathBuf::from(selector);
            if !path.is_file() {
                return Err(format!(
                    "performance selector is neither a manifest alias nor a file: {selector}"
                ));
            }
            match entry_for_path(manifest, &path) {
                Some(entry) => {
                    require_openslide_compatibility(entry, require_openslide)?;
                    slide_spec(entry, path)?
                }
                None if allow_custom_paths => SlideSpec::custom(path),
                None => {
                    return Err(format!(
                        "paired capture path is absent from the parity manifest: {}",
                        path.display()
                    ));
                }
            }
        };
        let identity = slide
            .path
            .canonicalize()
            .unwrap_or_else(|_| slide.path.clone());
        if seen.insert(identity) {
            selected.push(slide);
        }
    }
    Ok(selected)
}

fn require_openslide_compatibility(entry: &ManifestSlide, required: bool) -> Result<(), String> {
    if required && !entry.openslide_required {
        return Err(format!(
            "manifest alias {:?} is not declared OpenSlide-compatible",
            entry.alias
        ));
    }
    Ok(())
}

fn entry_for_path<'a>(manifest: &'a ParityManifest, path: &Path) -> Option<&'a ManifestSlide> {
    let requested = path.canonicalize().ok()?;
    manifest.slides.iter().find(|entry| {
        corpus::resolve_entry_path(entry, &workspace_root())
            .and_then(|candidate| candidate.canonicalize().ok())
            .as_ref()
            == Some(&requested)
    })
}

fn resolve_entry(entry: &ManifestSlide) -> Result<SlideSpec, String> {
    let path = resolve_entry_path(entry)?;
    slide_spec(entry, path)
}

fn slide_spec(entry: &ManifestSlide, path: PathBuf) -> Result<SlideSpec, String> {
    validate_manifest_sha256(entry, &path)?;
    Ok(SlideSpec {
        path,
        alias: entry.alias.clone(),
        format: entry.format.clone(),
        benchmark_group: benchmark_group(entry),
        manifest_sha256: (!entry.sha256.is_empty()).then(|| entry.sha256.clone()),
    })
}

fn validate_manifest_sha256(entry: &ManifestSlide, slide_path: &Path) -> Result<(), String> {
    if entry.sha256.is_empty() {
        return Ok(());
    }
    let source_path = if entry
        .url
        .rsplit('/')
        .next()
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".zip"))
    {
        corpus::source_archive_path(entry).ok_or_else(|| {
            format!(
                "manifest source archive for {:?} is missing; cannot verify SHA-256",
                entry.alias
            )
        })?
    } else {
        slide_path.to_path_buf()
    };
    let actual = wsi_rs_perf::sha256_file(&source_path)?;
    if actual.eq_ignore_ascii_case(&entry.sha256) {
        Ok(())
    } else {
        Err(format!(
            "manifest SHA-256 mismatch for {:?} at {}: expected {}, found {actual}",
            entry.alias,
            source_path.display(),
            entry.sha256
        ))
    }
}

fn benchmark_group(entry: &ManifestSlide) -> String {
    entry.benchmark_group()
}

fn resolve_entry_path(entry: &ManifestSlide) -> Result<PathBuf, String> {
    corpus::resolve_entry_path(entry, &workspace_root()).ok_or_else(|| {
        format!(
            "required performance fixture {:?} ({}) is missing from {}",
            entry.alias,
            entry.format,
            corpus::corpus_cache_dir().display()
        )
    })
}

#[cfg(test)]
fn cache_candidates(entry: &ManifestSlide) -> Vec<PathBuf> {
    corpus::cache_candidates(entry)
}

#[cfg(test)]
fn resolve_candidate(entry: &ManifestSlide, candidate: &Path) -> Option<PathBuf> {
    corpus::resolve_candidate(entry, candidate)
}

#[cfg(test)]
fn find_file_with_extension(root: &Path, extension: &str) -> Option<PathBuf> {
    corpus::find_file_with_extension(root, extension)
}

#[cfg(test)]
fn format_default_extension(format: &str) -> Option<&'static str> {
    corpus::format_default_extension(format)
}

fn default_manifest_path() -> PathBuf {
    workspace_root().join("tests/fixtures/parity_corpus.public.toml")
}

#[cfg(test)]
#[path = "tests/manifest.rs"]
mod tests;
