use super::super::*;
use crate::formats::tiff_family::container::tags;
use crate::formats::tiff_family::test_support::{build_tiff, SyntheticTag};

const TAG_MAKE: u16 = 271;
const ARGOS_METADATA_TAG: u16 = 65_000;

fn tiled_ifd(width: u32, height: u32) -> Vec<SyntheticTag> {
    vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, width),
        SyntheticTag::long(tags::IMAGE_LENGTH, height),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
    ]
}

#[test]
fn probe_detects_argos_before_generic_tiff() {
    let mut first = tiled_ifd(1024, 512);
    first.push(SyntheticTag::ascii(
        ARGOS_METADATA_TAG,
        "<Argos.Scan.Metadata><MinZ>0</MinZ><MaxZ>0</MaxZ></Argos.Scan.Metadata>",
    ));
    let file = build_tiff(&[first]);

    let result = TiffFamilyBackend::new().probe(file.path()).unwrap();
    assert!(result.detected);
    assert_eq!(result.vendor, "argos");
}

#[test]
fn probe_detects_huron_before_generic_tiff() {
    let mut first = tiled_ifd(1024, 512);
    first.push(SyntheticTag::ascii(TAG_MAKE, "Huron LE176"));
    first.push(SyntheticTag::ascii(
        tags::IMAGE_DESCRIPTION,
        "Scanner = LE176\nObjective = 20",
    ));
    let file = build_tiff(&[first]);

    let result = TiffFamilyBackend::new().probe(file.path()).unwrap();
    assert!(result.detected);
    assert_eq!(result.vendor, "huron");
}

#[test]
fn malformed_vendor_markers_fall_through_to_generic_tiff() {
    let mut wrong_argos = tiled_ifd(1024, 512);
    wrong_argos.push(SyntheticTag::ascii(
        ARGOS_METADATA_TAG,
        "<Not.Argos.Metadata><MinZ>0</MinZ><MaxZ>0</MaxZ></Not.Argos.Metadata>",
    ));
    let file = build_tiff(&[wrong_argos]);
    assert_eq!(
        TiffFamilyBackend::new().probe(file.path()).unwrap().vendor,
        "generic-tiff"
    );

    let mut wrong_huron = tiled_ifd(1024, 512);
    wrong_huron.push(SyntheticTag::ascii(TAG_MAKE, "Not Huron"));
    let file = build_tiff(&[wrong_huron]);
    assert_eq!(
        TiffFamilyBackend::new().probe(file.path()).unwrap().vendor,
        "generic-tiff"
    );
}
