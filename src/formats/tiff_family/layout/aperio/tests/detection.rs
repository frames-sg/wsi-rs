use super::super::*;
use super::fixtures::{build_aperio_tiff, SyntheticTag};
// ── Detection tests ──────────────────────────────────────────────

#[test]
fn detect_aperio_svs() {
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    assert!(interpreter.detect(&container));
}

#[test]
fn reject_non_aperio_tiled() {
    // Tiled but ImageDescription doesn't start with "Aperio"
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Generic TIFF"),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    assert!(!interpreter.detect(&container));
}

#[test]
fn reject_stripped_aperio_description() {
    // Has "Aperio" in description but no TILE_WIDTH tag
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    assert!(!interpreter.detect(&container));
}

#[test]
fn reject_no_description() {
    // Tiled but no ImageDescription tag at all
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    assert!(!interpreter.detect(&container));
}
