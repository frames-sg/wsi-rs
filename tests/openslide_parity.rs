//! OpenSlide compatibility-oracle parity test.

mod support;

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use wsi_rs::Slide;

use support::compare::{compare_rgba, tolerance_failure, Tolerance};
use support::corpus::{load_public, resolve_entry_path, CorpusEntry};
use support::oracles::{
    is_reference_oracle_unsupported, read_probe, top_left_probe, J2kOracle, Oracle, ReferenceOracle,
};

#[test]
#[ignore = "requires public parity corpus; run after scripts/parity-corpus-fetch.sh"]
fn preflight() {
    let strict_corpus = strict_corpus_required();
    let manifest = match load_public() {
        Ok(manifest) => manifest,
        Err(err) => {
            if strict_corpus {
                panic!("[preflight] public manifest unavailable in strict mode: {err}");
            }
            eprintln!("[preflight] public manifest unavailable: {err}; skipping");
            return;
        }
    };
    if std::env::var_os("WSI_RS_PARITY_ALIASES").is_none() {
        if let Err(error) = wsi_rs_test_support::corpus::validate_release_coverage(&manifest) {
            panic!("[preflight] release-corpus coverage is incomplete:\n{error}");
        }
    }

    #[cfg(feature = "parity-openslide")]
    let openslide = openslide_oracle();
    let mut checked = 0u32;
    let mut missing_slides = 0u32;
    let mut unsupported_reference = 0u32;
    let mut failures = Vec::new();

    for entry in &manifest.slides {
        let path: PathBuf = resolve_entry_path(entry);
        if !path.is_file() {
            missing_slides += 1;
            eprintln!(
                "[preflight] {} not present at {}; run scripts/parity-corpus-fetch.sh; skipping",
                entry.alias,
                path.display()
            );
            if strict_corpus {
                failures.push(format!(
                    "{}: corpus slide missing at {}",
                    entry.alias,
                    path.display()
                ));
            }
            continue;
        }
        if let Err(error) = validate_entry_sha256(entry, &path) {
            failures.push(error);
            continue;
        }
        validate_wsi_contract(entry, &path, &mut failures);

        let baseline = match ReferenceOracle.open(&path) {
            Ok(slide) => slide,
            Err(err) => {
                failures.push(format!("{}: reference open failed: {err}", entry.alias));
                continue;
            }
        };
        let j2k_report = match J2kOracle.open(&path) {
            Ok(slide) => Some(slide),
            Err(err) => {
                failures.push(format!("{}: j2k open failed: {err}", entry.alias));
                None
            }
        };

        if j2k_report.as_ref().is_some_and(|slide| {
            slide.level_count != baseline.level_count
                || slide.level_dimensions != baseline.level_dimensions
        }) {
            failures.push(format!(
                "{}: internal oracle geometry mismatch",
                entry.alias
            ));
        }

        #[cfg(feature = "parity-openslide")]
        let os_report = if entry.openslide_required {
            match openslide.open(&path) {
                Ok(os_slide) => {
                    if os_slide.level_count != baseline.level_count {
                        failures.push(format!(
                            "{}: OpenSlide level count mismatch openslide={} wsi_rs={}",
                            entry.alias, os_slide.level_count, baseline.level_count
                        ));
                    }
                    for (level, (ours, theirs)) in baseline
                        .level_dimensions
                        .iter()
                        .zip(os_slide.level_dimensions.iter())
                        .enumerate()
                    {
                        if !dimensions_within_one_pixel(*ours, *theirs) {
                            failures.push(format!(
                            "{}: OpenSlide dimension mismatch at level {level}: wsi_rs={ours:?} openslide={theirs:?}",
                            entry.alias
                        ));
                        }
                    }
                    Some(os_slide)
                }
                Err(err) => {
                    failures.push(format!(
                        "{}: required OpenSlide open failed: {err}",
                        entry.alias
                    ));
                    None
                }
            }
        } else {
            for divergence in &entry.oracle_divergences {
                eprintln!(
                    "[preflight] {} intentional oracle divergence: {divergence}",
                    entry.alias
                );
            }
            None
        };

        let required_levels = match entry.required_level_indices(baseline.level_count) {
            Ok(levels) => levels,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        for level in required_levels {
            let Some(probe) = top_left_probe(&baseline, level) else {
                eprintln!(
                    "[preflight] {} level={level}: no readable probe available; skipping decode",
                    entry.alias
                );
                failures.push(format!(
                    "{} level={level}: required decode has no readable probe",
                    entry.alias
                ));
                continue;
            };

            let baseline_buf = match read_probe(&baseline, probe) {
                Ok(buf) => Some(buf),
                Err(err) => {
                    eprintln!(
                        "[preflight] {} level={level}: baseline read failed: {err}; skipping",
                        entry.alias
                    );
                    if is_reference_oracle_unsupported(&err) {
                        unsupported_reference += 1;
                        None
                    } else {
                        failures.push(format!(
                            "{} level={level}: required reference read failed: {err}",
                            entry.alias
                        ));
                        continue;
                    }
                }
            };

            #[cfg(feature = "parity-openslide")]
            let mut j2k_buf = None;
            if let Some(ref j2k) = j2k_report {
                match read_probe(j2k, probe) {
                    Ok(sc_buf) => {
                        if let Some(ref baseline_buf) = baseline_buf {
                            let tolerance = tolerance_for_entry(entry);
                            let report = compare_rgba(
                                &sc_buf.pixels_rgba,
                                &baseline_buf.pixels_rgba,
                                tolerance,
                            );
                            eprintln!(
                                "[preflight] {} level={level} sc-vs-ref report max_abs={} mean_abs={:.4} psnr={:.2}dB equal_rate={:.4} passed={}",
                                entry.alias,
                                report.max_abs,
                                report.mean_abs,
                                report.psnr_db,
                                report.bytewise_equal_rate,
                                report.passed
                            );
                            // When OpenSlide is the declared compatibility authority, its
                            // comparison below adjudicates decoder-specific reference drift.
                            // Entries without that authority must pass the independent oracle.
                            if !entry.openslide_required {
                                record_comparison_failure(
                                    &format!("{} level={level}: j2k vs reference", entry.alias),
                                    &report,
                                    &mut failures,
                                );
                            }
                        } else {
                            eprintln!(
                                "[preflight] {} level={level}: reference oracle unsupported; j2k read succeeded without sc-vs-ref comparison",
                                entry.alias
                            );
                        }
                        #[cfg(feature = "parity-openslide")]
                        {
                            j2k_buf = Some(sc_buf.clone());
                        }
                    }
                    Err(err) => {
                        eprintln!(
                            "[preflight] {} level={level} j2k report read failed: {err}",
                            entry.alias
                        );
                        failures.push(format!(
                            "{} level={level}: required j2k read failed: {err}",
                            entry.alias
                        ));
                    }
                }
            }
            checked += 1;

            #[cfg(feature = "parity-openslide")]
            // `required_level_indices` keeps decode coverage broad; pixel parity follows the
            // manifest's explicit `must_decode` levels so synthetic/background-only levels do
            // not invent a stronger compatibility contract than the corpus declares.
            if entry.openslide_must_decode_level(level) {
                match os_report.as_ref().map_or_else(
                    || Err("OpenSlide comparison intentionally not required".to_string()),
                    |opened| read_probe(opened, probe),
                ) {
                    Ok(os_buf) => {
                        if let Some(ref baseline_buf) = baseline_buf {
                            let report = compare_rgba(
                                &baseline_buf.pixels_rgba,
                                &os_buf.pixels_rgba,
                                tolerance_for_entry(entry),
                            );
                            eprintln!(
                            "[preflight] {} level={level} reference-vs-openslide max_abs={} mean_abs={:.4} psnr={:.2}dB",
                            entry.alias, report.max_abs, report.mean_abs, report.psnr_db
                        );
                        }
                        if let Some(ref sc_buf) = j2k_buf {
                            let report = compare_rgba(
                                &sc_buf.pixels_rgba,
                                &os_buf.pixels_rgba,
                                tolerance_for_entry(entry),
                            );
                            eprintln!(
                            "[preflight] {} level={level} j2k-vs-openslide max_abs={} mean_abs={:.4} psnr={:.2}dB",
                            entry.alias, report.max_abs, report.mean_abs, report.psnr_db
                        );
                            record_comparison_failure(
                                &format!("{} level={level}: j2k vs OpenSlide", entry.alias),
                                &report,
                                &mut failures,
                            );
                        }
                    }
                    Err(err) => {
                        if entry.openslide_required {
                            failures.push(format!(
                                "{} level={level}: required OpenSlide read failed: {err}",
                                entry.alias
                            ));
                        }
                    }
                }
            }
        }
    }

    if missing_slides == 0 && checked == 0 {
        failures.push("preflight decoded zero tiles".to_string());
    }
    eprintln!(
        "[preflight] summary: checked={checked} unsupported_reference={unsupported_reference} missing_slides={missing_slides} failures={}",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "preflight failures:\n  {}",
        failures.join("\n  ")
    );
}

fn validate_entry_sha256(entry: &CorpusEntry, resolved_path: &Path) -> Result<(), String> {
    let source_path = if entry
        .url
        .rsplit('/')
        .next()
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".zip"))
    {
        wsi_rs_test_support::corpus::source_archive_path(entry).ok_or_else(|| {
            format!(
                "{}: source archive is missing, so its declared SHA-256 cannot be verified",
                entry.alias
            )
        })?
    } else {
        resolved_path.to_path_buf()
    };
    let file = File::open(&source_path).map_err(|error| {
        format!(
            "{}: open hash source {}: {error}",
            entry.alias,
            source_path.display()
        )
    })?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            format!(
                "{}: read hash source {}: {error}",
                entry.alias,
                source_path.display()
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual.eq_ignore_ascii_case(&entry.sha256) {
        Ok(())
    } else {
        Err(format!(
            "{}: SHA-256 mismatch for {}: expected {}, found {actual}",
            entry.alias,
            source_path.display(),
            entry.sha256
        ))
    }
}

fn validate_wsi_contract(entry: &CorpusEntry, path: &Path, failures: &mut Vec<String>) {
    let slide = match Slide::open(path) {
        Ok(slide) => slide,
        Err(error) => {
            failures.push(format!("{}: contract open failed: {error}", entry.alias));
            return;
        }
    };
    let dataset = slide.dataset();
    for property in &entry.required_properties {
        if dataset
            .properties
            .get(property)
            .is_none_or(|value| value.trim().is_empty())
        {
            failures.push(format!(
                "{}: required property {property:?} is missing",
                entry.alias
            ));
        }
    }
    let has_icc_profile = dataset.icc_profiles.values().any(|bytes| !bytes.is_empty())
        || dataset
            .source_icc_profiles
            .iter()
            .any(|profile| !profile.bytes.is_empty());
    if entry.require_icc_profile && !has_icc_profile {
        failures.push(format!(
            "{}: required pyramid ICC profile is missing",
            entry.alias
        ));
    }
    for name in entry.required_associated_images() {
        let Some(metadata) = dataset.associated_images.get(name) else {
            failures.push(format!(
                "{}: required associated image {name:?} is missing",
                entry.alias
            ));
            continue;
        };
        if metadata.dimensions.0 == 0 || metadata.dimensions.1 == 0 {
            failures.push(format!(
                "{}: associated image {name:?} has zero dimensions",
                entry.alias
            ));
            continue;
        }
        match slide.read_associated(name) {
            Ok(image) if (image.width(), image.height()) == metadata.dimensions => {}
            Ok(image) => failures.push(format!(
                "{}: associated image {name:?} geometry mismatch metadata={:?} decoded={}x{}",
                entry.alias,
                metadata.dimensions,
                image.width(),
                image.height()
            )),
            Err(error) => failures.push(format!(
                "{}: required associated image {name:?} failed to decode: {error}",
                entry.alias
            )),
        }
    }
    for name in &entry.required_associated_icc {
        match dataset.associated_images.get(name) {
            Some(image) if image.icc_profile().is_some() => {}
            Some(_) => failures.push(format!(
                "{}: associated image {name:?} is missing its required ICC profile",
                entry.alias
            )),
            None => failures.push(format!(
                "{}: associated ICC requirement names missing image {name:?}",
                entry.alias
            )),
        }
    }
}

fn strict_corpus_required() -> bool {
    true
}

fn record_comparison_failure(
    label: &str,
    report: &support::compare::CompareReport,
    failures: &mut Vec<String>,
) {
    let Some(failure) = tolerance_failure(label, report) else {
        return;
    };
    failures.push(failure);
}

fn tolerance_for_entry(entry: &CorpusEntry) -> Tolerance {
    if entry.lossless {
        Tolerance::EXACT
    } else if entry
        .codecs
        .iter()
        .any(|codec| matches!(codec.as_str(), "j2k" | "htj2k"))
    {
        Tolerance::TOLERANT
    } else {
        Tolerance::JPEG_TIGHT
    }
}

#[cfg(feature = "parity-openslide")]
fn dimensions_within_one_pixel(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0.abs_diff(b.0) <= 1 && a.1.abs_diff(b.1) <= 1
}

#[cfg(feature = "parity-openslide")]
fn openslide_oracle() -> support::oracles::OpenSlideOracle {
    let lib = support::openslide_shim::try_load()
        .expect("libopenslide is required when parity-openslide is enabled");
    support::oracles::OpenSlideOracle { lib }
}
