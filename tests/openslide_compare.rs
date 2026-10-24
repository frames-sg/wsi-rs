mod openslide_test_support;
mod support;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use openslide_test_support::OpenSlide;
use support::compare::{compare_rgba, Tolerance};
use wsi_rs::{PlaneSelection, Slide};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PixelCompareMode {
    Exact,
    JpegTolerance,
    Jp2kTolerance,
}

#[derive(Clone, Copy)]
struct RegionCase {
    label: &'static str,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
}

#[test]
#[ignore = "requires WSI_RS_OPENSLIDE_COMPARE_PATHS and libopenslide"]
fn compare_against_openslide_for_env_paths() {
    let raw_paths = env::var_os("WSI_RS_OPENSLIDE_COMPARE_PATHS")
        .expect("WSI_RS_OPENSLIDE_COMPARE_PATHS is required for OpenSlide comparison");

    let paths: Vec<PathBuf> = env::split_paths(&raw_paths)
        .map(resolve_compare_path)
        .collect();
    assert!(
        !paths.is_empty(),
        "WSI_RS_OPENSLIDE_COMPARE_PATHS was set but contained no valid paths"
    );

    for path in paths {
        compare_slide(&path);
    }
}

#[test]
#[ignore = "requires WSI_RS_APERIO_JPEG_RGB_PATH and libopenslide"]
fn aperio_jpeg_rgb_pyramid_levels_match_openslide() {
    let path = aperio_jpeg_rgb_regression_path()
        .expect("WSI_RS_APERIO_JPEG_RGB_PATH is required for the Aperio RGB regression");
    let handle = Slide::open(&path).expect("open Aperio RGB regression slide with wsi_rs");
    let openslide = OpenSlide::open(&path)
        .unwrap_or_else(|err| panic!("open Aperio RGB regression with OpenSlide: {err}"));
    let level_count = handle.dataset().scenes[0].series[0].levels.len();
    let max_level = level_count.saturating_sub(1).min(3);

    for level in 1..=max_level {
        let req = support::region_request(
            0,
            0,
            level as u32,
            PlaneSelection::default(),
            0,
            0,
            240,
            240,
        );
        let ours = handle
            .read_region_rgba(&req)
            .unwrap_or_else(|err| panic!("wsi_rs read level {level} failed: {err}"))
            .into_raw();
        let theirs = openslide
            .read_region_rgba(0, 0, level as i32, 240, 240)
            .unwrap_or_else(|err| panic!("OpenSlide read level {level} failed: {err}"));
        assert_rgba_with_tolerance(
            &path,
            RegionCase {
                label: "aperio_jpeg_rgb_top_left",
                x: 0,
                y: 0,
                w: 240,
                h: 240,
            },
            &ours,
            &theirs,
            12,
            200,
            "Aperio JPEG/RGB pyramid",
        );
    }
}

fn aperio_jpeg_rgb_regression_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("WSI_RS_APERIO_JPEG_RGB_PATH") {
        return Some(resolve_compare_path(PathBuf::from(path)));
    }

    None
}

fn resolve_compare_path(path: PathBuf) -> PathBuf {
    let resolved = if path.is_absolute() || path.is_file() {
        path
    } else {
        support::corpus::local_testdata_candidates(&path)
            .into_iter()
            .find(|candidate| candidate.is_file())
            .unwrap_or(path)
    };
    std::fs::canonicalize(&resolved).unwrap_or(resolved)
}

#[test]
fn normalize_property_value_strips_outer_single_quotes() {
    assert_eq!(normalize_property_value("'hamamatsu'"), "hamamatsu");
    assert_eq!(normalize_property_value("hamamatsu"), "hamamatsu");
    assert_eq!(
        normalize_property_value("'Aperio Image Library\\nJPEG/RGB'"),
        "Aperio Image Library\\nJPEG/RGB"
    );
}

#[test]
fn parse_openslide_properties_keeps_multiline_values_together() {
    let props = parse_openslide_properties(
        "openslide.vendor: 'aperio'\n\
         openslide.comment: 'Aperio Image Library v12.0.11\n\
         123952x83886 JPEG/RGB Q=30|Time = 17:53:49'\n\
         openslide.level-count: '4'\n",
    );

    assert_eq!(
        props.get("openslide.vendor").map(String::as_str),
        Some("aperio")
    );
    assert_eq!(
        props.get("openslide.comment").map(String::as_str),
        Some("Aperio Image Library v12.0.11\n123952x83886 JPEG/RGB Q=30|Time = 17:53:49")
    );
    assert_eq!(
        props.get("openslide.level-count").map(String::as_str),
        Some("4")
    );
}

#[test]
fn pixel_compare_mode_uses_jpeg_tolerance_for_public_generic_tiff() {
    let path = Path::new("downloads/openslide-testdata/Generic-TIFF/CMU-1.tiff");
    let props = HashMap::from([("openslide.vendor".to_string(), "generic-tiff".to_string())]);

    assert_eq!(
        pixel_compare_mode(path, &props),
        PixelCompareMode::JpegTolerance
    );
}

#[test]
fn pixel_compare_mode_keeps_jp2k_separate_from_jpeg_tiff_rules() {
    let path = Path::new("downloads/openslide-testdata/Aperio/JP2K-33003-1.svs");
    let props = HashMap::from([(
        "openslide.comment".to_string(),
        "Aperio Image Library v12.0.11 JPEG2000/RGB".to_string(),
    )]);

    assert_eq!(
        pixel_compare_mode(path, &props),
        PixelCompareMode::Jp2kTolerance
    );
}

#[test]
fn pixel_compare_mode_uses_jpeg_tolerance_for_mirax() {
    let path = Path::new("downloads/openslide-testdata/Mirax/Mirax2.2-1.mrxs");
    let props = HashMap::from([("openslide.vendor".to_string(), "mirax".to_string())]);

    assert_eq!(
        pixel_compare_mode(path, &props),
        PixelCompareMode::JpegTolerance
    );
}

#[test]
fn pixel_compare_mode_keeps_exact_for_unclassified_tiff() {
    let path = Path::new("custom-slide.tiff");
    let props = HashMap::new();

    assert_eq!(pixel_compare_mode(path, &props), PixelCompareMode::Exact);
}

fn compare_slide(path: &Path) {
    let handle = Slide::open(path).expect("open slide with wsi_rs");
    let openslide = OpenSlide::open(path).expect("open slide with OpenSlide");
    let openslide_props = openslide_properties(path);
    let ours = handle.dataset();
    let series = &ours.scenes[0].series[0];

    compare_exact_property(
        path,
        &openslide_props,
        ours.properties.vendor(),
        "openslide.vendor",
    );
    compare_exact_property(
        path,
        &openslide_props,
        ours.properties.quickhash1(),
        "openslide.quickhash-1",
    );
    compare_float_property(
        path,
        &openslide_props,
        ours.properties.get("openslide.mpp-x"),
        "openslide.mpp-x",
    );
    compare_float_property(
        path,
        &openslide_props,
        ours.properties.get("openslide.mpp-y"),
        "openslide.mpp-y",
    );
    compare_float_property(
        path,
        &openslide_props,
        ours.properties.get("openslide.objective-power"),
        "openslide.objective-power",
    );

    if let Some(level_count) = openslide_props.get("openslide.level-count") {
        let theirs = level_count
            .parse::<usize>()
            .expect("parse openslide.level-count");
        assert_eq!(
            series.levels.len(),
            theirs,
            "level-count mismatch for {}",
            path.display()
        );
    }

    for (idx, level) in series.levels.iter().enumerate() {
        let width_key = format!("openslide.level[{idx}].width");
        let height_key = format!("openslide.level[{idx}].height");
        if let Some(width) = openslide_props.get(&width_key) {
            assert_eq!(
                level.dimensions.0.to_string(),
                *width,
                "width mismatch for {} key {}",
                path.display(),
                width_key
            );
        }
        if let Some(height) = openslide_props.get(&height_key) {
            assert_eq!(
                level.dimensions.1.to_string(),
                *height,
                "height mismatch for {} key {}",
                path.display(),
                height_key
            );
        }
    }

    compare_associated_images(path, &handle, &openslide);
    compare_level0_regions(path, &handle, &openslide, &openslide_props);
}

fn compare_associated_images(path: &Path, handle: &Slide, openslide: &OpenSlide) {
    let ours = handle
        .dataset()
        .associated_images
        .iter()
        .map(|(name, image)| (name.clone(), image.dimensions))
        .collect::<BTreeMap<_, _>>();
    let theirs = openslide
        .associated_names()
        .into_iter()
        .map(|name| {
            let dims = openslide
                .associated_dimensions(&name)
                .unwrap_or_else(|err| {
                    panic!(
                        "associated dimension read failed for {}: {err}",
                        path.display()
                    )
                });
            (name, dims)
        })
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        ours,
        theirs,
        "associated image metadata mismatch for {}",
        path.display()
    );
}

fn compare_level0_regions(
    path: &Path,
    handle: &Slide,
    openslide: &OpenSlide,
    openslide_props: &HashMap<String, String>,
) {
    let level0 = &handle.dataset().scenes[0].series[0].levels[0];
    let mode = pixel_compare_mode(path, openslide_props);
    for region in representative_regions(level0.dimensions) {
        let req = support::region_request(
            0,
            0,
            0,
            PlaneSelection::default(),
            region.x,
            region.y,
            region.w,
            region.h,
        );
        let ours = handle
            .read_region_rgba(&req)
            .unwrap_or_else(|err| {
                panic!(
                    "wsi_rs read_region_rgba failed for {} {}: {err}",
                    path.display(),
                    region.label
                )
            })
            .into_raw();
        let theirs = openslide
            .read_region_rgba(region.x, region.y, 0, region.w, region.h)
            .unwrap_or_else(|err| {
                panic!(
                    "OpenSlide read_region failed for {} {}: {err}",
                    path.display(),
                    region.label
                )
            });

        match mode {
            PixelCompareMode::Exact => {
                assert_rgba_exact(path, region, &ours, &theirs);
            }
            PixelCompareMode::JpegTolerance => {
                assert_rgba_with_tolerance(path, region, &ours, &theirs, 12, 200, "JPEG")
            }
            PixelCompareMode::Jp2kTolerance => {
                assert_rgba_with_tolerance(path, region, &ours, &theirs, 50, 1600, "JP2K")
            }
        }
    }
}

fn openslide_properties(path: &Path) -> HashMap<String, String> {
    let output = Command::new("openslide-show-properties")
        .arg(path)
        .output()
        .expect("run openslide-show-properties");
    assert!(
        output.status.success(),
        "openslide-show-properties failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("decode openslide stdout");
    parse_openslide_properties(&stdout)
}

fn parse_openslide_properties(stdout: &str) -> HashMap<String, String> {
    let mut props = HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_value = String::new();

    let flush_current = |props: &mut HashMap<String, String>,
                         current_name: &mut Option<String>,
                         current_value: &mut String| {
        let Some(name) = current_name.take() else {
            current_value.clear();
            return;
        };
        let value = normalize_property_value(current_value.trim());
        if !value.is_empty() {
            props.insert(name, value.to_string());
        }
        current_value.clear();
    };

    for line in stdout.lines() {
        if let Some((name, value)) = parse_property_line(line) {
            let name = name.trim();
            if !name.is_empty() {
                flush_current(&mut props, &mut current_name, &mut current_value);
                current_name = Some(name.to_string());
                current_value.push_str(value.trim());
                continue;
            }
        }

        if current_name.is_some() {
            if !current_value.is_empty() {
                current_value.push('\n');
            }
            current_value.push_str(line.trim());
        }
    }

    flush_current(&mut props, &mut current_name, &mut current_value);
    props
}

fn parse_property_line(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.split_once(':')?;
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '[' | ']'))
    {
        return None;
    }
    Some((name, value))
}

fn normalize_property_value(value: &str) -> &str {
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn pixel_compare_mode(path: &Path, openslide_props: &HashMap<String, String>) -> PixelCompareMode {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if is_jp2k_slide(openslide_props) {
        return PixelCompareMode::Jp2kTolerance;
    }
    if openslide_props
        .get("openslide.vendor")
        .is_some_and(|vendor| vendor == "dicom")
    {
        return match openslide_props
            .get("dicom.TransferSyntaxUID")
            .map(String::as_str)
        {
            Some("1.2.840.10008.1.2.4.50") => PixelCompareMode::JpegTolerance,
            Some("1.2.840.10008.1.2.4.90" | "1.2.840.10008.1.2.4.91") => {
                PixelCompareMode::Jp2kTolerance
            }
            _ => PixelCompareMode::Exact,
        };
    }
    if extension == "ndpi" || extension == "vms" || extension == "vmu" || extension == "mrxs" {
        return PixelCompareMode::JpegTolerance;
    }

    if extension == "svs" || extension == "scn" || extension == "bif" {
        return PixelCompareMode::JpegTolerance;
    }

    if (extension == "tif" || extension == "tiff")
        && openslide_props
            .get("openslide.vendor")
            .is_some_and(|vendor| matches!(vendor.as_str(), "generic-tiff" | "philips" | "trestle"))
    {
        return PixelCompareMode::JpegTolerance;
    }

    PixelCompareMode::Exact
}

fn is_jp2k_slide(openslide_props: &HashMap<String, String>) -> bool {
    let descriptor = openslide_props
        .get("openslide.comment")
        .or_else(|| openslide_props.get("tiff.ImageDescription"));
    descriptor.is_some_and(|value| value.contains("J2K/") || value.contains("JPEG2000"))
}

fn representative_regions(dimensions: (u64, u64)) -> Vec<RegionCase> {
    let width = dimensions.0.min(u64::from(u32::MAX)) as u32;
    let height = dimensions.1.min(u64::from(u32::MAX)) as u32;
    let span = width.min(height).clamp(1, 128);

    let candidates = [
        RegionCase {
            label: "top_left",
            x: 0,
            y: 0,
            w: span,
            h: span,
        },
        RegionCase {
            label: "center",
            x: i64::from(width.saturating_sub(span) / 2),
            y: i64::from(height.saturating_sub(span) / 2),
            w: span,
            h: span,
        },
        RegionCase {
            label: "bottom_right",
            x: i64::from(width.saturating_sub(span)),
            y: i64::from(height.saturating_sub(span)),
            w: span,
            h: span,
        },
    ];

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for region in candidates {
        if seen.insert((region.x, region.y, region.w, region.h)) {
            out.push(region);
        }
    }
    out
}

fn assert_rgba_with_tolerance(
    path: &Path,
    region: RegionCase,
    ours: &[u8],
    theirs: &[u8],
    max_allowed_delta: u8,
    avg_allowed_delta_x100: u64,
    label: &str,
) {
    let report = compare_rgba(
        ours,
        theirs,
        Tolerance {
            max_abs: max_allowed_delta,
            mean_abs: avg_allowed_delta_x100 as f64 / 100.0,
        },
    );
    assert!(
        report.passed,
        "{label} pixel mismatch for {} region {}: max_abs={} mean_abs={} alpha_exact={}",
        path.display(),
        region.label,
        report.max_abs,
        report.mean_abs,
        report.alpha_exact
    );
}

fn assert_rgba_exact(path: &Path, region: RegionCase, ours: &[u8], theirs: &[u8]) {
    let report = compare_rgba(ours, theirs, Tolerance::EXACT);
    assert!(
        report.passed,
        "pixel mismatch for {} region {} at ({}, {}) {}x{}: {report:?}",
        path.display(),
        region.label,
        region.x,
        region.y,
        region.w,
        region.h
    );
}

fn compare_exact_property(
    path: &Path,
    openslide_props: &HashMap<String, String>,
    ours: Option<&str>,
    key: &str,
) {
    let Some(theirs) = openslide_props.get(key) else {
        return;
    };
    assert_eq!(
        ours,
        Some(theirs.as_str()),
        "property mismatch for {} key {}",
        path.display(),
        key
    );
}

fn compare_float_property(
    path: &Path,
    openslide_props: &HashMap<String, String>,
    ours: Option<&str>,
    key: &str,
) {
    let Some(theirs) = openslide_props.get(key) else {
        return;
    };
    let Some(ours) = ours else {
        panic!("missing property for {} key {}", path.display(), key);
    };

    let ours = ours.parse::<f64>().expect("parse wsi_rs float property");
    let theirs = theirs
        .parse::<f64>()
        .expect("parse openslide float property");
    assert!(
        (ours - theirs).abs() <= 1e-6,
        "float property mismatch for {} key {}: ours={} theirs={}",
        path.display(),
        key,
        ours,
        theirs
    );
}

#[test]
fn region_parity_rejects_coverage_mismatch_even_when_rgb_matches() {
    let path = Path::new("coverage-probe.svs");
    let region = RegionCase {
        label: "coverage",
        x: 0,
        y: 0,
        w: 1,
        h: 1,
    };
    let opaque = [10, 20, 30, 255];
    let transparent = [10, 20, 30, 0];
    assert!(std::panic::catch_unwind(|| {
        assert_rgba_with_tolerance(path, region, &opaque, &transparent, 4, 100, "coverage");
    })
    .is_err());
    assert!(std::panic::catch_unwind(|| {
        assert_rgba_exact(path, region, &opaque, &transparent);
    })
    .is_err());
}
