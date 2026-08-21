use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

const CORPUS_CACHE_ENV: &str = "WSI_RS_PARITY_CORPUS_CACHE";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CorpusEntry {
    #[serde(default)]
    pub name: String,
    pub alias: String,
    #[serde(default)]
    pub path: String,
    pub format: String,
    #[serde(default)]
    pub codecs: Vec<String>,
    #[serde(default)]
    pub benchmark_group: String,
    #[serde(default)]
    pub must_decode: Vec<String>,
    #[serde(default = "default_true")]
    pub openslide_required: bool,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub redistributable: bool,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub citation: String,
    #[serde(default)]
    pub phi_reviewed: bool,
    #[serde(default)]
    pub tolerant_regions: Vec<String>,
    #[serde(default)]
    pub expected_failures: Vec<String>,
    #[serde(default)]
    pub url: String,
}

impl CorpusEntry {
    pub fn must_decode_level(&self, level: u32) -> bool {
        self.must_decode.iter().any(|item| {
            item == "base" && level == 0
                || item
                    .strip_prefix("level")
                    .and_then(|number| number.parse::<u32>().ok())
                    == Some(level)
        })
    }

    pub fn openslide_must_decode_level(&self, level: u32) -> bool {
        self.openslide_required && self.must_decode_level(level)
    }

    pub fn expected_failure(&self, pair: &str, level: u32) -> bool {
        let numbered = format!("{pair}:level{level}");
        let base = (level == 0).then(|| format!("{pair}:base"));
        self.expected_failures
            .iter()
            .any(|item| item == &numbered || base.as_ref() == Some(item))
    }

    pub fn benchmark_group(&self) -> String {
        if !self.benchmark_group.is_empty() {
            return self.benchmark_group.clone();
        }
        let mut codecs = self.codecs.clone();
        codecs.sort();
        codecs.dedup();
        if codecs.is_empty() {
            self.format.clone()
        } else {
            format!("{}/{}", self.format, codecs.join("+"))
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct CorpusManifest {
    #[serde(rename = "slide", default)]
    pub slides: Vec<CorpusEntry>,
}

pub fn parse_manifest(text: &str) -> Result<CorpusManifest, String> {
    let manifest: CorpusManifest =
        toml::from_str(text).map_err(|error| format!("manifest parse: {error}"))?;
    let mut aliases = BTreeSet::new();
    for slide in &manifest.slides {
        if slide.alias.is_empty() || slide.format.is_empty() {
            return Err("manifest slide alias and format must be non-empty".into());
        }
        if !aliases.insert(slide.alias.as_str()) {
            return Err(format!("duplicate manifest alias {:?}", slide.alias));
        }
    }
    Ok(manifest)
}

pub fn load_manifest(path: &Path) -> Result<CorpusManifest, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read manifest {}: {error}", path.display()))?;
    parse_manifest(&text).map_err(|error| format!("{}: {error}", path.display()))
}

pub fn corpus_cache_dir() -> PathBuf {
    if let Some(path) = std::env::var_os(CORPUS_CACHE_ENV) {
        return PathBuf::from(path);
    }
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from(".cache/slideviewer/parity-corpus"),
        |home| {
            PathBuf::from(home)
                .join(".cache")
                .join("slideviewer")
                .join("parity-corpus")
        },
    )
}

pub fn resolve_entry_path(entry: &CorpusEntry, workspace_root: &Path) -> Option<PathBuf> {
    if !entry.path.is_empty() {
        let configured = PathBuf::from(&entry.path);
        for candidate in [configured.clone(), workspace_root.join(&configured)] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    cache_candidates(entry)
        .into_iter()
        .find_map(|candidate| resolve_candidate(entry, &candidate))
}

pub fn source_archive_path(entry: &CorpusEntry) -> Option<PathBuf> {
    cache_candidates(entry).into_iter().find(|candidate| {
        candidate.is_file()
            && candidate
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    })
}

pub fn cache_candidates(entry: &CorpusEntry) -> Vec<PathBuf> {
    let cache = corpus_cache_dir();
    let mut candidates = vec![
        cache.join(&entry.alias),
        cache.join(format!("{}.d", entry.alias)),
    ];
    if let Some(extension) = format_default_extension(&entry.format) {
        candidates.push(cache.join(format!("{}.{}", entry.alias, extension)));
    }
    if let Some(name) = entry.url.rsplit('/').next().filter(|name| !name.is_empty()) {
        candidates.push(cache.join(name));
    }
    candidates
}

pub fn resolve_candidate(entry: &CorpusEntry, candidate: &Path) -> Option<PathBuf> {
    if candidate.is_file() {
        if matches!(entry.format.as_str(), "hamamatsu_vms" | "mirax")
            && candidate
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            return None;
        }
        return Some(candidate.to_path_buf());
    }
    let extension = match entry.format.as_str() {
        "hamamatsu_vms" => "vms",
        "mirax" => "mrxs",
        "dicom" => "dcm",
        _ => return None,
    };
    find_file_with_extension(candidate, extension)
}

pub fn find_file_with_extension(root: &Path, extension: &str) -> Option<PathBuf> {
    let mut entries = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_file()
            && path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file_with_extension(&path, extension) {
                return Some(found);
            }
        }
    }
    None
}

pub fn format_default_extension(format: &str) -> Option<&'static str> {
    match format {
        "aperio" => Some("svs"),
        "leica" => Some("scn"),
        "ventana" => Some("bif"),
        "philips_tiff" | "tiff" => Some("tif"),
        "ndpi" => Some("ndpi"),
        "hamamatsu_vms" | "mirax" => Some("zip"),
        "dicom" => Some("dcm"),
        "zeiss_czi" => Some("czi"),
        _ => None,
    }
}

pub fn apply_alias_filter(manifest: &mut CorpusManifest, raw_aliases: Option<&str>) {
    let Some(raw_aliases) = raw_aliases else {
        return;
    };
    let aliases = raw_aliases
        .split(|character: char| character == ',' || character == ';' || character.is_whitespace())
        .filter(|alias| !alias.is_empty())
        .collect::<HashSet<_>>();
    if !aliases.is_empty() {
        manifest
            .slides
            .retain(|entry| aliases.contains(entry.alias.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_rejects_duplicate_aliases() {
        let error = parse_manifest(
            "[[slide]]\nalias='same'\nformat='aperio'\n[[slide]]\nalias='same'\nformat='ndpi'\n",
        )
        .unwrap_err();
        assert!(error.contains("duplicate manifest alias"));
    }

    #[test]
    fn benchmark_group_is_stable_and_deduplicated() {
        let entry = CorpusEntry {
            name: String::new(),
            alias: "slide".into(),
            path: String::new(),
            format: "aperio".into(),
            codecs: vec!["jpeg".into(), "j2k".into(), "jpeg".into()],
            benchmark_group: String::new(),
            must_decode: Vec::new(),
            openslide_required: true,
            source: String::new(),
            license: String::new(),
            redistributable: false,
            sha256: String::new(),
            citation: String::new(),
            phi_reviewed: false,
            tolerant_regions: Vec::new(),
            expected_failures: Vec::new(),
            url: String::new(),
        };
        assert_eq!(entry.benchmark_group(), "aperio/j2k+jpeg");
    }
}
