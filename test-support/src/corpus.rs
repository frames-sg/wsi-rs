use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CORPUS_CACHE_ENV: &str = "WSI_RS_PARITY_CORPUS_CACHE";
const PARITY_ALIASES_ENV: &str = "WSI_RS_PARITY_ALIASES";

/// Formats covered by the available public release samples.
///
/// Each vendor-specific interpreter needs its own evidence. VMU/NGR has only
/// synthetic fixtures and is excluded until a real public sample is available.
pub const REQUIRED_RELEASE_FORMATS: &[&str] = &[
    "aperio",
    "argos",
    "dicom",
    "hamamatsu_vms",
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

/// Per-channel limits for an RGBA pixel comparison. Alpha is always compared
/// exactly; these limits apply to the colour channels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Tolerance {
    pub max_abs: u8,
    pub mean_abs: f64,
}

impl Tolerance {
    pub const EXACT: Self = Self {
        max_abs: 0,
        mean_abs: 0.0,
    };

    pub const JPEG_TIGHT: Self = Self {
        max_abs: 1,
        mean_abs: 0.05,
    };

    pub const JPEG_DECODER_COMPAT: Self = Self {
        max_abs: 3,
        mean_abs: 0.125,
    };

    pub const TOLERANT: Self = Self {
        max_abs: 4,
        mean_abs: 1.0,
    };
}

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

    /// The pixel tolerance every comparison of this entry against a reference
    /// uses: the parity tests and the release performance gate.
    pub fn pixel_tolerance(&self) -> Tolerance {
        if self.lossless {
            Tolerance::EXACT
        } else if self
            .codecs
            .iter()
            .any(|codec| matches!(codec.as_str(), "j2k" | "htj2k"))
        {
            Tolerance::TOLERANT
        } else if self.format == "huron" && self.codecs.iter().any(|codec| codec == "jpeg") {
            Tolerance::JPEG_DECODER_COMPAT
        } else {
            Tolerance::JPEG_TIGHT
        }
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
        || PathBuf::from(".cache/wsi-rs/parity-corpus"),
        |home| {
            PathBuf::from(home)
                .join(".cache")
                .join("wsi-rs")
                .join("parity-corpus")
        },
    )
}

/// Where a local test-data path such as
/// `downloads/openslide-testdata/Aperio/CMU-1.svs` may live, in search
/// order: under the workspace root, its parent, then its grandparent.
pub fn local_testdata_candidates(relative: &Path) -> Vec<PathBuf> {
    workspace_root()
        .ancestors()
        .take(3)
        .map(|root| root.join(relative))
        .collect()
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("test-support is a workspace member")
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

/// Check the file the entry's `sha256` describes: the fetched archive when
/// the entry's URL names a zip, otherwise the resolved slide. An entry that
/// declares no SHA-256 is an error.
pub fn verify_entry_sha256(entry: &CorpusEntry, resolved: &Path) -> Result<(), String> {
    if entry.sha256.is_empty() {
        return Err(format!("{}: manifest declares no SHA-256", entry.alias));
    }
    let source = if url_names_zip(entry) {
        source_archive_path(entry).ok_or_else(|| {
            format!(
                "{}: source archive is missing, so its declared SHA-256 cannot be verified",
                entry.alias
            )
        })?
    } else {
        resolved.to_path_buf()
    };
    let actual = sha256_file(&source).map_err(|error| format!("{}: {error}", entry.alias))?;
    if actual.eq_ignore_ascii_case(&entry.sha256) {
        Ok(())
    } else {
        Err(format!(
            "{}: SHA-256 mismatch for {}: expected {}, found {actual}",
            entry.alias,
            source.display(),
            entry.sha256
        ))
    }
}

/// The lower-case hexadecimal SHA-256 of a file's contents.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let file = File::open(path)
        .map_err(|err| format!("failed to open {} for hashing: {err}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut checksum = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| format!("failed to hash {}: {err}", path.display()))?;
        if read == 0 {
            break;
        }
        checksum.update(&buffer[..read]);
    }
    Ok(format!("{:x}", checksum.finalize()))
}

fn url_names_zip(entry: &CorpusEntry) -> bool {
    entry
        .url
        .rsplit('/')
        .next()
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".zip"))
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
        if url_names_zip(entry) {
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
            "hamamatsu_vms" | "hamamatsu_vmu" | "mirax" | "trestle" | "olympus_vsi"
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
        "trestle" => "tif",
        "olympus_vsi" => "vsi",
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

/// The corpus aliases `WSI_RS_PARITY_ALIASES` selects, or `None` when it
/// selects the whole corpus. See [`parse_aliases`] for the grammar.
pub fn selected_aliases() -> Option<BTreeSet<String>> {
    parse_aliases(
        std::env::var_os(PARITY_ALIASES_ENV)
            .as_deref()
            .map(|value| value.to_string_lossy())
            .as_deref(),
    )
}

/// Whether `WSI_RS_PARITY_ALIASES` selects `alias`. Every alias is selected
/// when the variable selects the whole corpus.
pub fn alias_selected(alias: &str) -> bool {
    selected_aliases().is_none_or(|aliases| aliases.contains(alias))
}

/// Parse an alias list. Aliases are separated by commas, semicolons or
/// whitespace. An absent value, or one with no aliases, selects the whole
/// corpus and returns `None`.
pub fn parse_aliases(raw: Option<&str>) -> Option<BTreeSet<String>> {
    let aliases = raw?
        .split(|character: char| character == ',' || character == ';' || character.is_whitespace())
        .filter(|alias| !alias.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    (!aliases.is_empty()).then_some(aliases)
}

/// Keep only the selected aliases; `None` keeps the whole manifest.
pub fn apply_alias_filter(manifest: &mut CorpusManifest, aliases: Option<&BTreeSet<String>>) {
    if let Some(aliases) = aliases {
        manifest
            .slides
            .retain(|entry| aliases.contains(&entry.alias));
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
    fn pixel_tolerance_follows_lossiness_codec_and_vendor() {
        let entry = |format: &str, codec: &str, lossless: bool| CorpusEntry {
            alias: format!("{format}-{codec}"),
            format: format.into(),
            codecs: vec![codec.into()],
            lossless,
            ..CorpusEntry::default()
        };
        for (format, codec, lossless, expected) in [
            ("raw_jp2k", "j2k", true, Tolerance::EXACT),
            ("raw_jp2k", "htj2k", true, Tolerance::EXACT),
            ("svcache", "zstd", true, Tolerance::EXACT),
            ("raw_jp2k", "j2k", false, Tolerance::TOLERANT),
            ("dicom", "htj2k", false, Tolerance::TOLERANT),
            ("huron", "jpeg", false, Tolerance::JPEG_DECODER_COMPAT),
            ("aperio", "jpeg", false, Tolerance::JPEG_TIGHT),
            ("dicom", "jpeg-baseline", false, Tolerance::JPEG_TIGHT),
            ("huron", "none", false, Tolerance::JPEG_TIGHT),
        ] {
            assert_eq!(
                entry(format, codec, lossless).pixel_tolerance(),
                expected,
                "{format}/{codec} lossless={lossless}"
            );
        }
    }

    #[test]
    fn alias_lists_split_on_commas_semicolons_and_whitespace() {
        let parsed = parse_aliases(Some(" svs-001,ndpi-001;mirax-001\n vms-001,,")).unwrap();
        assert_eq!(
            parsed.iter().map(String::as_str).collect::<Vec<_>>(),
            ["mirax-001", "ndpi-001", "svs-001", "vms-001"]
        );
        for whole_corpus in [None, Some(""), Some(" "), Some(",;")] {
            assert_eq!(parse_aliases(whole_corpus), None, "{whole_corpus:?}");
        }
    }

    #[test]
    fn public_manifest_satisfies_release_coverage() {
        let manifest = parse_manifest(include_str!(
            "../../tests/fixtures/parity_corpus.public.toml"
        ))
        .unwrap();
        validate_release_coverage(&manifest).unwrap();
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

        for (format, extension) in [
            ("mirax", "mrxs"),
            ("trestle", "tif"),
            ("olympus_vsi", "vsi"),
        ] {
            let archive = directory.join(format!("{format}.zip"));
            std::fs::write(&archive, b"archive").unwrap();
            let extracted = directory.join(format);
            std::fs::create_dir_all(&extracted).unwrap();
            let slide = extracted.join(format!("slide.{extension}"));
            std::fs::write(&slide, b"fixture").unwrap();
            let entry = CorpusEntry {
                alias: format.into(),
                format: format.into(),
                ..CorpusEntry::default()
            };
            assert_eq!(
                resolve_candidate(&entry, &archive),
                None,
                "archive is not a slide"
            );
            assert_eq!(resolve_candidate(&entry, &extracted), Some(slide));
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn entry_sha256_checks_the_resolved_slide_and_requires_a_digest() {
        let directory =
            std::env::temp_dir().join(format!("wsi-rs-corpus-sha-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let slide = directory.join("slide.svs");
        std::fs::write(&slide, b"abc").unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_file(&slide).unwrap(), abc);
        assert!(sha256_file(&directory.join("missing"))
            .unwrap_err()
            .contains("failed to open"));

        let entry = |sha256: &str| CorpusEntry {
            alias: "slide".into(),
            format: "aperio".into(),
            sha256: sha256.into(),
            ..CorpusEntry::default()
        };
        verify_entry_sha256(&entry(abc), &slide).unwrap();
        verify_entry_sha256(&entry(&abc.to_ascii_uppercase()), &slide).unwrap();
        let mismatch = verify_entry_sha256(&entry(&"0".repeat(64)), &slide).unwrap_err();
        assert!(mismatch.contains("slide: SHA-256 mismatch"), "{mismatch}");
        assert!(verify_entry_sha256(&entry(""), &slide)
            .unwrap_err()
            .contains("declares no SHA-256"));
        let unreadable = verify_entry_sha256(&entry(abc), &directory.join("missing")).unwrap_err();
        assert!(
            unreadable.starts_with("slide: failed to open"),
            "{unreadable}"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn entry_sha256_of_a_zip_url_hashes_the_fetched_archive() {
        let entry = CorpusEntry {
            alias: "wsi-rs-no-such-archive-alias".into(),
            format: "mirax".into(),
            sha256: "a".repeat(64),
            url: "https://example.invalid/wsi-rs-no-such-archive.zip".into(),
            ..CorpusEntry::default()
        };
        let error = verify_entry_sha256(&entry, Path::new("Cargo.toml")).unwrap_err();
        assert!(error.contains("source archive is missing"), "{error}");
    }

    #[test]
    fn local_testdata_is_searched_from_the_workspace_root_outwards() {
        let relative = Path::new("downloads/openslide-testdata/Aperio/CMU-1.svs");
        let candidates = local_testdata_candidates(relative);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        assert_eq!(
            candidates,
            [
                root.join(relative),
                root.parent().unwrap().join(relative),
                root.parent().unwrap().parent().unwrap().join(relative),
            ]
        );
        assert!(local_testdata_candidates(Path::new("Cargo.toml"))[0]
            .parent()
            .unwrap()
            .join("test-support")
            .is_dir());
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
