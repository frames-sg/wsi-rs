//! DICOM-specific parity harness.

mod support;

#[cfg(feature = "parity-openslide")]
use support::corpus::CorpusEntry;
use support::corpus::{load_public, resolve_entry_path};
use support::oracles::{read_probe, top_left_probe, J2kOracle, Oracle};

#[test]
#[ignore = "requires public parity corpus; run after scripts/parity-corpus-fetch.sh"]
fn dicom_public_corpus_decodes_with_wsi_rs() {
    let strict_corpus = strict_corpus_required();
    let manifest = match load_public() {
        Ok(manifest) => manifest,
        Err(err) => {
            if strict_corpus {
                panic!("[dicom-parity] public manifest unavailable in strict mode: {err}");
            }
            eprintln!("[dicom-parity] manifest unavailable: {err}; skipping");
            return;
        }
    };

    let mut checked = 0u32;
    let mut missing_slides = 0u32;
    let mut failures = Vec::new();
    for entry in manifest
        .slides
        .iter()
        .filter(|entry| entry.format == "dicom")
    {
        let path = resolve_entry_path(entry);
        if !path.is_file() {
            missing_slides += 1;
            eprintln!(
                "[dicom-parity] {} missing at {}; skipping",
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
        let slide = match J2kOracle.open(&path) {
            Ok(slide) => slide,
            Err(err) => {
                failures.push(format!("{}: open wsi_rs DICOM: {err}", entry.alias));
                continue;
            }
        };
        if entry
            .codecs
            .iter()
            .any(|codec| matches!(codec.as_str(), "j2k" | "htj2k"))
            && !entry.openslide_required
            && entry.oracle_divergences.is_empty()
        {
            failures.push(format!(
                "{}: missing intentional OpenSlide color-divergence record",
                entry.alias
            ));
        }
        let required_levels = match entry.required_level_indices(slide.level_count) {
            Ok(levels) => levels,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        for level in required_levels {
            let Some(probe) = top_left_probe(&slide, level) else {
                failures.push(format!("{} level={level}: no readable probe", entry.alias));
                continue;
            };
            match read_probe(&slide, probe) {
                Ok(tile) => {
                    checked += 1;
                    eprintln!(
                        "[dicom-parity] {} level={level}: decoded {}x{}",
                        entry.alias, tile.width, tile.height
                    );
                }
                Err(err) => failures.push(format!("{} level={level}: decode: {err}", entry.alias)),
            }
        }
    }

    if missing_slides == 0 && checked == 0 {
        failures.push("DICOM parity decoded zero corpus tiles".to_string());
    }
    eprintln!(
        "[dicom-parity] checked={checked} missing_slides={missing_slides} failures={}",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(feature = "parity-openslide")]
#[test]
#[ignore = "requires public parity corpus and libopenslide"]
fn dicom_public_corpus_matches_openslide_within_tolerance() {
    use support::compare::{compare_rgba, tolerance_failure};
    use support::oracles::OpenSlideOracle;

    let lib = support::openslide_shim::try_load()
        .expect("libopenslide is required for DICOM OpenSlide parity");
    let openslide = OpenSlideOracle { lib };
    let manifest = load_public().expect("load public manifest");
    let required_entries = manifest
        .slides
        .iter()
        .filter(|entry| {
            entry.format == "dicom" && entry.openslide_required && !entry.must_decode.is_empty()
        })
        .collect::<Vec<_>>();
    if required_entries.is_empty() {
        eprintln!("[dicom-parity] no public DICOM entries require an OpenSlide oracle");
        return;
    }
    let mut checked = 0u32;
    let mut failures = Vec::new();

    for entry in required_entries {
        let path = resolve_entry_path(entry);
        if !path.is_file() {
            failures.push(format!(
                "{}: corpus slide missing at {}",
                entry.alias,
                path.display()
            ));
            continue;
        }
        let ours = match J2kOracle.open(&path) {
            Ok(slide) => slide,
            Err(err) => {
                failures.push(format!("{}: open wsi_rs DICOM: {err}", entry.alias));
                continue;
            }
        };
        let theirs = match openslide.open(&path) {
            Ok(slide) => slide,
            Err(err) => {
                failures.push(format!("{}: open OpenSlide: {err}", entry.alias));
                continue;
            }
        };
        if ours.level_count != theirs.level_count
            || ours.level_dimensions != theirs.level_dimensions
        {
            failures.push(format!(
                "{}: exact geometry mismatch wsi_rs={:?} OpenSlide={:?}",
                entry.alias, ours.level_dimensions, theirs.level_dimensions
            ));
            continue;
        }
        let required_levels = match entry.required_level_indices(ours.level_count) {
            Ok(levels) => levels,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        for level in required_levels {
            let Some(probe) = top_left_probe(&ours, level) else {
                failures.push(format!("{} level={level}: no readable probe", entry.alias));
                continue;
            };
            let ours_buf = match read_probe(&ours, probe) {
                Ok(buf) => buf,
                Err(err) => {
                    failures.push(format!("{} level={level}: read wsi_rs: {err}", entry.alias));
                    continue;
                }
            };
            let theirs_buf = match read_probe(&theirs, probe) {
                Ok(buf) => buf,
                Err(err) => {
                    failures.push(format!(
                        "{} level={level}: read OpenSlide: {err}",
                        entry.alias
                    ));
                    continue;
                }
            };
            if (ours_buf.width, ours_buf.height) != (theirs_buf.width, theirs_buf.height) {
                failures.push(format!(
                    "{} level={level}: exact probe geometry mismatch wsi_rs={}x{} OpenSlide={}x{}",
                    entry.alias,
                    ours_buf.width,
                    ours_buf.height,
                    theirs_buf.width,
                    theirs_buf.height
                ));
                continue;
            }
            let report = compare_rgba(
                &ours_buf.pixels_rgba,
                &theirs_buf.pixels_rgba,
                tolerance_for_entry(entry),
            );
            eprintln!(
                "[dicom-parity] {} level={level}: max_abs={} mean_abs={:.4} passed={}",
                entry.alias, report.max_abs, report.mean_abs, report.passed
            );
            if let Some(failure) = tolerance_failure(
                &format!("{} level={level}: wsi_rs vs OpenSlide", entry.alias),
                &report,
            ) {
                failures.push(failure);
            }
            checked += 1;
        }
    }

    if checked == 0 {
        failures.push("DICOM OpenSlide parity checked zero tiles".to_string());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn strict_corpus_required() -> bool {
    true
}

#[cfg(feature = "parity-openslide")]
fn tolerance_for_entry(entry: &CorpusEntry) -> support::compare::Tolerance {
    if entry.lossless {
        support::compare::Tolerance::EXACT
    } else if entry
        .codecs
        .iter()
        .any(|codec| matches!(codec.as_str(), "j2k" | "htj2k"))
    {
        support::compare::Tolerance::TOLERANT
    } else {
        support::compare::Tolerance::JPEG_TIGHT
    }
}
