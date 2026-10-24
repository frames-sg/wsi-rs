use super::*;

const MANIFEST: &str = r#"
[[slide]]
alias = "fixture-jp2k"
format = "raw_jp2k"
codecs = ["j2k"]
path = "tests/fixtures/jp2k/rgb_nomct.j2k"
must_decode = ["base"]
openslide_required = true
sha256 = "c06bf36f197a8057cbe0d496682c1eab5483c6667db8c64e236d087c747896ab"

[[slide]]
alias = "wsi-only"
format = "dicom"
codecs = ["j2k"]
path = "tests/fixtures/jp2k/rgb_nomct.j2k"
must_decode = ["base"]
openslide_required = false

[[slide]]
alias = "non-gating"
format = "dicom"
codecs = ["jpeg-progressive"]
path = "tests/fixtures/jp2k/rgb_nomct.j2k"
must_decode = []
openslide_required = true
"#;

#[test]
fn manifest_alias_selection_records_format_and_expected_hash() {
    let manifest = corpus::parse_manifest(MANIFEST).expect("manifest");

    let slides = resolve_manifest_slides(&manifest, &["fixture-jp2k".to_string()], true, false)
        .expect("manifest slide");

    assert_eq!(slides.len(), 1);
    assert_eq!(slides[0].alias, "fixture-jp2k");
    assert_eq!(slides[0].format, "raw_jp2k");
    assert_eq!(slides[0].benchmark_group, "raw_jp2k/j2k");
    assert_eq!(
        slides[0].pixel_tolerance,
        Some(wsi_rs_test_support::corpus::Tolerance::TOLERANT)
    );
    assert_eq!(
        slides[0].manifest_sha256.as_deref(),
        Some("c06bf36f197a8057cbe0d496682c1eab5483c6667db8c64e236d087c747896ab")
    );
    assert!(slides[0].path.is_file());
}

#[test]
fn manifest_selection_rejects_a_fixture_with_the_wrong_sha256() {
    let manifest = corpus::parse_manifest(&MANIFEST.replace(
        "c06bf36f197a8057cbe0d496682c1eab5483c6667db8c64e236d087c747896ab",
        "0000000000000000000000000000000000000000000000000000000000000000",
    ))
    .expect("manifest");

    let error = resolve_manifest_slides(&manifest, &["fixture-jp2k".to_string()], true, false)
        .expect_err("fixture identity mismatch must fail closed");

    assert!(error.contains("SHA-256 mismatch"), "{error}");
    assert!(error.contains("fixture-jp2k"), "{error}");
}

#[test]
fn default_manifest_selection_excludes_non_gating_rows() {
    let manifest = corpus::parse_manifest(MANIFEST).expect("manifest");

    let slides = resolve_manifest_slides(&manifest, &[], false, false).expect("gating slides");

    assert_eq!(
        slides
            .iter()
            .map(|slide| slide.alias.as_str())
            .collect::<Vec<_>>(),
        vec!["fixture-jp2k", "wsi-only"]
    );
}

#[test]
fn paired_manifest_selection_rejects_wsi_only_aliases() {
    let manifest = corpus::parse_manifest(MANIFEST).expect("manifest");

    let error = resolve_manifest_slides(&manifest, &["wsi-only".to_string()], true, false)
        .expect_err("wsi-only row must not enter paired capture");

    assert!(error.contains("not declared OpenSlide-compatible"));
}

#[test]
fn public_manifest_derives_separate_aperio_codec_groups() {
    let text = std::fs::read_to_string(default_manifest_path()).expect("public manifest");
    let manifest = corpus::parse_manifest(&text).expect("public manifest parse");
    let group = |alias: &str| {
        let entry = manifest
            .slides
            .iter()
            .find(|entry| entry.alias == alias)
            .expect("manifest alias");
        entry.benchmark_group()
    };

    assert_eq!(group("svs-001"), "aperio/jpeg");
    assert_eq!(group("svs-jp2k-001"), "aperio/j2k");
}

#[test]
fn an_empty_manifest_selects_no_slides() {
    let empty = corpus::parse_manifest("").expect("empty manifest parses");
    assert!(resolve_manifest_slides(&empty, &[], false, false)
        .unwrap_err()
        .contains("selected no slides"));
}

#[test]
fn explicit_paths_are_matched_deduplicated_or_rejected_by_policy() {
    let manifest = corpus::parse_manifest(MANIFEST).expect("manifest");
    let fixture = workspace_root().join("tests/fixtures/jp2k/rgb_nomct.j2k");
    let selector = fixture.display().to_string();

    let matched = resolve_manifest_slides(
        &manifest,
        &[selector.clone(), selector.clone()],
        false,
        true,
    )
    .expect("matched path");
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].alias, "fixture-jp2k");

    let custom_path = workspace_root().join("Cargo.toml");
    let custom_selector = custom_path.display().to_string();
    let custom = resolve_manifest_slides(
        &manifest,
        std::slice::from_ref(&custom_selector),
        false,
        true,
    )
    .expect("custom path");
    assert_eq!(custom[0].format, "custom");
    assert_eq!(custom[0].alias, "Cargo");
    assert!(
        resolve_manifest_slides(&manifest, &[custom_selector], false, false)
            .unwrap_err()
            .contains("absent from the parity manifest")
    );
    assert!(
        resolve_manifest_slides(&manifest, &["missing-selector".into()], false, true)
            .unwrap_err()
            .contains("neither a manifest alias nor a file")
    );
}
