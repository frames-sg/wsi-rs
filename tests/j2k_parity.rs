//! J2k CPU vs reference parity harness.

mod support;

use std::path::Path;

use support::compare::{compare_rgba, tolerance_failure};
use support::corpus::{load_public_unfiltered, resolve_present};
use support::oracles::{read_probe, top_left_probe, J2kOracle, Oracle};

#[test]
#[ignore = "requires public parity corpus; run after scripts/parity-corpus-fetch.sh"]
fn j2k_cpu_vs_reference_within_tolerance() {
    let manifest = load_public_unfiltered()
        .unwrap_or_else(|err| panic!("[sc-parity] public manifest unavailable: {err}"));
    let mut checked = 0u32;
    let mut missing_slides = 0u32;
    let mut failures = Vec::new();

    for entry in manifest.slides.iter().filter(|entry| {
        entry.format == "raw_jp2k"
            && entry
                .codecs
                .iter()
                .any(|codec| matches!(codec.as_str(), "j2k" | "htj2k"))
    }) {
        let Some(path) = resolve_present(entry, &mut failures) else {
            missing_slides += 1;
            continue;
        };

        let sc = match J2kOracle.open(&path) {
            Ok(slide) => slide,
            Err(err) => {
                failures.push(format!("{}: open j2k: {err}", entry.alias));
                continue;
            }
        };
        let required_levels = match entry.required_level_indices(sc.level_count) {
            Ok(levels) => levels,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        for level in required_levels {
            let Some(probe) = top_left_probe(&sc, level) else {
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
            let ref_buf = match read_ppm_oracle(&reference_ppm_path(&path, level)) {
                Ok(buf) => buf,
                Err(ppm_error) => {
                    failures.push(format!(
                        "{} level={level}: raw JP2K PPM oracle failed: {ppm_error}",
                        entry.alias
                    ));
                    continue;
                }
            };
            if (sc_buf.width, sc_buf.height) != (ref_buf.width, ref_buf.height) {
                failures.push(format!(
                    "{} level={level}: exact geometry mismatch j2k={}x{} reference={}x{}",
                    entry.alias, sc_buf.width, sc_buf.height, ref_buf.width, ref_buf.height
                ));
                continue;
            }
            let tolerance = entry.pixel_tolerance();
            let cmp = compare_rgba(&sc_buf.pixels_rgba, &ref_buf.pixels_rgba, tolerance);
            eprintln!(
                "[sc-parity] {} level={level}: max_abs={} mean_abs={:.4} passed={}",
                entry.alias, cmp.max_abs, cmp.mean_abs, cmp.passed
            );
            failures.extend(tolerance_failure(
                &format!("{} level={level}: j2k vs reference", entry.alias),
                &cmp,
            ));
            checked += 1;
        }
    }

    if missing_slides == 0 && checked == 0 {
        failures
            .push("raw JP2K parity decoded zero independently reference-backed tiles".to_string());
    }
    eprintln!(
        "[sc-parity] checked={checked} missing_slides={missing_slides} failures={}",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Level 0 is compared with `{stem}.ppm`; reduced level `k` with the
/// independent OpenJPEG `-r k` decode in `{stem}.r{k}.ppm`.
fn reference_ppm_path(codestream: &Path, level: u32) -> std::path::PathBuf {
    if level == 0 {
        return codestream.with_extension("ppm");
    }
    let stem = codestream
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    codestream.with_file_name(format!("{stem}.r{level}.ppm"))
}

fn read_ppm_oracle(path: &Path) -> Result<support::oracles::TileBuffer, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read decoded PPM {}: {error}", path.display()))?;
    let rgb = image::load_from_memory_with_format(&bytes, image::ImageFormat::Pnm)
        .map_err(|error| format!("decode PPM oracle {}: {error}", path.display()))?
        .to_rgb8();
    let (width, height) = rgb.dimensions();
    let mut pixels_rgba = Vec::with_capacity(rgb.as_raw().len() / 3 * 4);
    for pixel in rgb.as_raw().as_chunks::<3>().0 {
        pixels_rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    Ok(support::oracles::TileBuffer {
        pixels_rgba,
        width,
        height,
    })
}
