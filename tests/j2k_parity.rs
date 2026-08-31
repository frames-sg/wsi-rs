//! J2k CPU vs reference parity harness.

mod support;

use std::path::Path;

use support::compare::{compare_rgba, tolerance_failure, Tolerance};
use support::corpus::{load_public, resolve_entry_path, CorpusEntry};
use support::oracles::{
    is_reference_oracle_unsupported, read_probe, top_left_probe, J2kOracle, Oracle, ReferenceOracle,
};

#[test]
#[ignore = "requires public parity corpus; run after scripts/parity-corpus-fetch.sh"]
fn j2k_cpu_vs_reference_within_tolerance() {
    let strict_corpus = strict_corpus_required();
    let manifest = match load_public() {
        Ok(manifest) => manifest,
        Err(err) => {
            if strict_corpus {
                panic!("[sc-parity] public manifest unavailable in strict mode: {err}");
            }
            eprintln!("[sc-parity] manifest unavailable: {err}; skipping");
            return;
        }
    };
    let mut checked = 0u32;
    let mut missing_slides = 0u32;
    let mut unsupported_reference = 0u32;
    let mut failures = Vec::new();

    for entry in manifest.slides.iter().filter(|entry| {
        entry
            .codecs
            .iter()
            .any(|codec| matches!(codec.as_str(), "j2k" | "htj2k"))
    }) {
        let path = resolve_entry_path(entry);
        if !path.is_file() {
            missing_slides += 1;
            eprintln!(
                "[sc-parity] {} missing at {}; skipping",
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

        let sc = match J2kOracle.open(&path) {
            Ok(slide) => slide,
            Err(err) => {
                failures.push(format!("{}: open j2k: {err}", entry.alias));
                continue;
            }
        };
        let reference = match ReferenceOracle.open(&path) {
            Ok(slide) => slide,
            Err(err) => {
                failures.push(format!("{}: open reference: {err}", entry.alias));
                continue;
            }
        };

        if sc.level_count != reference.level_count
            || sc.level_dimensions != reference.level_dimensions
        {
            failures.push(format!(
                "{}: internal oracle geometry mismatch",
                entry.alias
            ));
            continue;
        }
        let required_levels = match entry.required_level_indices(sc.level_count) {
            Ok(levels) => levels,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        for level in required_levels {
            let Some(probe) = top_left_probe(&reference, level) else {
                failures.push(format!(
                    "{} level={level}: required decode has no readable probe",
                    entry.alias
                ));
                continue;
            };
            let sc_buf = match read_probe(&sc, probe) {
                Ok(buf) => buf,
                Err(err) => {
                    eprintln!(
                        "[sc-parity] {} level={level}: read j2k failed: {err}; skipping",
                        entry.alias
                    );
                    failures.push(format!(
                        "{} level={level}: required j2k read failed: {err}",
                        entry.alias
                    ));
                    continue;
                }
            };
            let ref_buf = match read_probe(&reference, probe) {
                Ok(buf) => buf,
                Err(err) => {
                    if entry.format == "raw_jp2k" {
                        match read_ppm_oracle(&path.with_extension("ppm")) {
                            Ok(buf) => buf,
                            Err(ppm_error) => {
                                failures.push(format!(
                                    "{} level={level}: raw JP2K PPM oracle failed: {ppm_error}",
                                    entry.alias
                                ));
                                continue;
                            }
                        }
                    } else {
                        eprintln!(
                            "[sc-parity] {} level={level}: read reference failed: {err}; skipping",
                            entry.alias
                        );
                        if is_reference_oracle_unsupported(&err) {
                            // Packaged JP2K is covered by the format parity test. This oracle
                            // independently decodes JPEG only; the tracked raw JP2K fixture has
                            // a PPM oracle, and the zero-comparison check below keeps that required.
                            unsupported_reference += 1;
                        } else {
                            failures.push(format!(
                                "{} level={level}: required reference read failed: {err}",
                                entry.alias
                            ));
                        }
                        continue;
                    }
                }
            };
            if (sc_buf.width, sc_buf.height) != (ref_buf.width, ref_buf.height) {
                failures.push(format!(
                    "{} level={level}: exact geometry mismatch j2k={}x{} reference={}x{}",
                    entry.alias, sc_buf.width, sc_buf.height, ref_buf.width, ref_buf.height
                ));
                continue;
            }
            let tolerance = tolerance_for_entry(entry);
            let cmp = compare_rgba(&sc_buf.pixels_rgba, &ref_buf.pixels_rgba, tolerance);
            eprintln!(
                "[sc-parity] {} level={level}: max_abs={} mean_abs={:.4} passed={}",
                entry.alias, cmp.max_abs, cmp.mean_abs, cmp.passed
            );
            record_comparison_failure(
                &format!("{} level={level}: j2k vs reference", entry.alias),
                &cmp,
                &mut failures,
            );
            checked += 1;
        }
    }

    if missing_slides == 0 && checked == 0 {
        failures.push("j2k parity decoded zero independently reference-backed tiles".to_string());
    }
    eprintln!(
        "[sc-parity] checked={checked} unsupported_reference={unsupported_reference} missing_slides={missing_slides} failures={}",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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

fn read_ppm_oracle(path: &Path) -> Result<support::oracles::TileBuffer, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read decoded PPM {}: {error}", path.display()))?;
    let rgb = image::load_from_memory_with_format(&bytes, image::ImageFormat::Pnm)
        .map_err(|error| format!("decode PPM oracle {}: {error}", path.display()))?
        .to_rgb8();
    let (width, height) = rgb.dimensions();
    let mut pixels_rgba = Vec::with_capacity(rgb.as_raw().len() / 3 * 4);
    for pixel in rgb.as_raw().chunks_exact(3) {
        pixels_rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    Ok(support::oracles::TileBuffer {
        pixels_rgba,
        width,
        height,
    })
}
