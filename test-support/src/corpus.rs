use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

const CORPUS_CACHE_ENV: &str = "WSI_RS_PARITY_CORPUS_CACHE";

/// Format evidence required before the built-in registry can be released.
///
/// This is deliberately the retained format inventory rather than the smaller
/// backend inventory: one TIFF sample cannot establish production readiness for
/// all vendor-specific TIFF interpreters.
pub const REQUIRED_RELEASE_FORMATS: &[&str] = &[
    "aperio",
    "argos",
    "dicom",
    "hamamatsu_vms",
    "hamamatsu_vmu",
    "huron",
    "leica",
    "mirax",
    "ndpi",
    "olympus_vsi",
    "philips_tiff",
    "raw_jp2k",
    "svcache",
    "tiff",
    "trestle",
    "ventana",
    "zeiss_zvi",
];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub notes: String,
    #[serde(default)]
    pub lossless: bool,
    #[serde(default)]
    pub required_properties: Vec<String>,
    #[serde(default)]
    pub require_icc_profile: bool,
    #[serde(default)]
    pub required_associated_icc: Vec<String>,
    #[serde(default)]
    pub color_paths: Vec<String>,
    #[serde(default)]
    pub oracle_divergences: Vec<String>,
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

    pub fn required_level_indices(&self, level_count: u32) -> Result<Vec<u32>, String> {
        if level_count == 0 {
            return Err(format!("{}: slide has no levels", self.alias));
        }

        // Every release sample exercises the base, a representative reduced
        // level, and the highest level. For one- and two-level slides these
        // naturally collapse to the available distinct indices.
        let mut required = BTreeSet::from([0, level_count - 1]);
        if level_count > 2 {
            required.insert((level_count - 1) / 2);
        }
        for requirement in &self.must_decode {
            let level = match requirement.as_str() {
                "base" => Some(0),
                "reduced" => (level_count > 1).then_some(1),
                "highest" => Some(level_count - 1),
                value => value
                    .strip_prefix("level")
                    .and_then(|number| number.parse::<u32>().ok()),
            };
            if let Some(level) = level {
                if level >= level_count {
                    return Err(format!(
                        "{}: required level {level} is outside level count {level_count}",
                        self.alias
                    ));
                }
                required.insert(level);
            }
        }
        Ok(required.into_iter().collect())
    }

    pub fn required_associated_images(&self) -> impl Iterator<Item = &str> {
        self.must_decode.iter().filter_map(|requirement| {
            (!is_level_requirement(requirement)).then_some(requirement.as_str())
        })
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

fn is_level_requirement(value: &str) -> bool {
    matches!(value, "base" | "reduced" | "highest")
        || value.strip_prefix("level").is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        })
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

/// Validate evidence required by release preflight without pretending missing
/// public samples exist. Callers receive one combined diagnostic so corpus work
/// can be completed without repeated gate runs.
pub fn validate_release_coverage(manifest: &CorpusManifest) -> Result<(), String> {
    let mut failures = Vec::new();
    let present_formats = manifest
        .slides
        .iter()
        .map(|slide| slide.format.as_str())
        .collect::<BTreeSet<_>>();

    for format in REQUIRED_RELEASE_FORMATS {
        if !present_formats.contains(format) {
            failures.push(format!(
                "missing representative release-corpus evidence for {format}"
            ));
        }
    }

    let mut has_icc_evidence = false;
    let mut has_associated_evidence = false;
    let mut has_lossless_jp2k_evidence = false;
    let mut has_lossless_htj2k_evidence = false;
    let mut has_progressive_dicom_jpeg_evidence = false;
    let mut dicom_jp2k_color_paths = BTreeSet::new();
    for slide in &manifest.slides {
        let prefix = &slide.alias;
        if slide.codecs.is_empty() {
            failures.push(format!("{prefix}: codecs must not be empty"));
        }
        if !slide.must_decode_level(0) {
            failures.push(format!("{prefix}: must_decode must include base"));
        }
        if slide.source.is_empty()
            || slide.license.is_empty()
            || slide.citation.is_empty()
            || slide.sha256.is_empty()
        {
            failures.push(format!(
                "{prefix}: source, license, citation, and sha256 are required"
            ));
        }
        if slide.sha256.len() != 64 || !slide.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            failures.push(format!(
                "{prefix}: sha256 must be 64 hexadecimal characters"
            ));
        }
        if slide.path.is_empty() && slide.url.is_empty() {
            failures.push(format!("{prefix}: either path or url is required"));
        }
        if !slide.redistributable || !slide.phi_reviewed {
            failures.push(format!(
                "{prefix}: public evidence must be redistributable and PHI-reviewed"
            ));
        }
        if !slide
            .required_properties
            .iter()
            .any(|property| property == "openslide.vendor")
        {
            failures.push(format!(
                "{prefix}: required_properties must include openslide.vendor"
            ));
        }
        has_icc_evidence |= slide.require_icc_profile || !slide.required_associated_icc.is_empty();
        has_associated_evidence |= slide.required_associated_images().next().is_some();
        has_lossless_jp2k_evidence |=
            slide.lossless && slide.codecs.iter().any(|codec| codec == "j2k");
        has_lossless_htj2k_evidence |=
            slide.lossless && slide.codecs.iter().any(|codec| codec == "htj2k");
        has_progressive_dicom_jpeg_evidence |=
            slide.format == "dicom" && slide.codecs.iter().any(|codec| codec == "jpeg-progressive");

        if slide.format == "dicom"
            && slide
                .codecs
                .iter()
                .any(|codec| matches!(codec.as_str(), "j2k" | "htj2k"))
        {
            dicom_jp2k_color_paths.extend(slide.color_paths.iter().map(String::as_str));
            if !slide.openslide_required && slide.oracle_divergences.is_empty() {
                failures.push(format!(
                    "{prefix}: an intentional DICOM JP2K oracle divergence must be recorded when OpenSlide is not required"
                ));
            }
        }
    }

    if !has_icc_evidence {
        failures.push("missing ICC-profile release-corpus evidence".into());
    }
    if !has_associated_evidence {
        failures.push("missing associated-image release-corpus evidence".into());
    }
    if !has_lossless_jp2k_evidence {
        failures.push("missing lossless JP2K byte-exact release-corpus evidence".into());
    }
    if !has_lossless_htj2k_evidence {
        failures.push("missing lossless HTJ2K byte-exact release-corpus evidence".into());
    }
    if !has_progressive_dicom_jpeg_evidence {
        failures.push("missing real SOF2 progressive-JPEG DICOM release-corpus evidence".into());
    }
    for color_path in ["RGB", "YBR_RCT", "YBR_ICT"] {
        if !dicom_jp2k_color_paths.contains(color_path) {
            failures.push(format!(
                "missing independent DICOM JP2K {color_path} release-corpus evidence"
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
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
        if name.to_ascii_lowercase().ends_with(".zip") {
            let fetched_archive = cache.join(format!("{}.zip", entry.alias));
            if !candidates.contains(&fetched_archive) {
                candidates.push(fetched_archive);
            }
        }
        candidates.push(cache.join(name));
    }
    candidates
}

pub fn resolve_candidate(entry: &CorpusEntry, candidate: &Path) -> Option<PathBuf> {
    if candidate.is_file() {
        if matches!(
            entry.format.as_str(),
            "hamamatsu_vms" | "hamamatsu_vmu" | "mirax"
        ) && candidate
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
        "hamamatsu_vmu" => "vmu",
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
        "argos" => Some("avs"),
        "huron" => Some("tif"),
        "leica" => Some("scn"),
        "ventana" => Some("bif"),
        "philips_tiff" | "tiff" => Some("tif"),
        "ndpi" => Some("ndpi"),
        "hamamatsu_vms" | "hamamatsu_vmu" | "mirax" => Some("zip"),
        "dicom" => Some("dcm"),
        "olympus_vsi" => Some("vsi"),
        "raw_jp2k" => Some("j2k"),
        "svcache" => Some("svcache"),
        "zeiss_zvi" => Some("zvi"),
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
            alias: "slide".into(),
            format: "aperio".into(),
            codecs: vec!["jpeg".into(), "j2k".into(), "jpeg".into()],
            ..CorpusEntry::default()
        };
        assert_eq!(entry.benchmark_group(), "aperio/j2k+jpeg");
    }

    #[test]
    fn release_validation_exercises_codec_and_metadata_evidence() {
        let entry = CorpusEntry {
            alias: "dicom-evidence".into(),
            path: "slide.dcm".into(),
            format: "dicom".into(),
            codecs: vec!["j2k".into(), "htj2k".into(), "jpeg-progressive".into()],
            must_decode: vec!["base".into(), "label".into()],
            openslide_required: false,
            source: "public".into(),
            license: "test".into(),
            redistributable: true,
            sha256: "a".repeat(64),
            citation: "test citation".into(),
            phi_reviewed: true,
            lossless: true,
            required_properties: vec!["openslide.vendor".into()],
            require_icc_profile: true,
            color_paths: vec!["RGB".into(), "YBR_RCT".into(), "YBR_ICT".into()],
            oracle_divergences: vec!["documented reference limitation".into()],
            ..CorpusEntry::default()
        };
        let error = validate_release_coverage(&CorpusManifest {
            slides: vec![entry],
        })
        .unwrap_err();
        assert!(error.contains("missing representative release-corpus evidence"));
        assert!(!error.contains("lossless JP2K"));
        assert!(!error.contains("lossless HTJ2K"));
        assert!(!error.contains("progressive-JPEG"));
    }

    #[test]
    fn manifest_loading_and_zip_resolution_report_file_boundaries() {
        let directory =
            std::env::temp_dir().join(format!("wsi-rs-corpus-tests-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let missing = directory.join("missing.toml");
        assert!(load_manifest(&missing)
            .unwrap_err()
            .contains("failed to read"));

        let malformed = directory.join("malformed.toml");
        std::fs::write(&malformed, "not = [valid").unwrap();
        assert!(load_manifest(&malformed)
            .unwrap_err()
            .contains("manifest parse"));

        let archive = directory.join("mirax.zip");
        std::fs::write(&archive, b"archive").unwrap();
        let entry = CorpusEntry {
            alias: "mirax".into(),
            format: "mirax".into(),
            ..CorpusEntry::default()
        };
        assert_eq!(resolve_candidate(&entry, &archive), None);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn zip_url_candidates_include_the_fetcher_archive_name() {
        let entry = CorpusEntry {
            alias: "dicom-jp2k-001".into(),
            format: "dicom".into(),
            url: "https://example.invalid/CMU-1-JP2K-33005.zip".into(),
            ..CorpusEntry::default()
        };

        assert!(cache_candidates(&entry)
            .iter()
            .any(|candidate| candidate.ends_with("dicom-jp2k-001.zip")));
    }
}
