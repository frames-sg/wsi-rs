use super::*;
use crate::formats::tiff_family::test_support::{build_tiff, SyntheticTag};

fn tiled(width: u32, height: u32, compression: u16) -> Vec<SyntheticTag> {
    vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, width),
        SyntheticTag::long(tags::IMAGE_LENGTH, height),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, compression),
        SyntheticTag::long(tags::TILE_OFFSETS, 8),
        SyntheticTag::long(tags::TILE_BYTE_COUNTS, 1),
    ]
}

fn stripped(width: u32, height: u32, subfile_type: Option<u32>) -> Vec<SyntheticTag> {
    let mut tags = vec![
        SyntheticTag::long(
            crate::formats::tiff_family::container::tags::IMAGE_WIDTH,
            width,
        ),
        SyntheticTag::long(
            crate::formats::tiff_family::container::tags::IMAGE_LENGTH,
            height,
        ),
        SyntheticTag::short(crate::formats::tiff_family::container::tags::COMPRESSION, 1),
        SyntheticTag::long(
            crate::formats::tiff_family::container::tags::STRIP_OFFSETS,
            8,
        ),
        SyntheticTag::long(
            crate::formats::tiff_family::container::tags::STRIP_BYTE_COUNTS,
            1,
        ),
    ];
    if let Some(value) = subfile_type {
        tags.push(SyntheticTag::long(TAG_SUBFILE_TYPE, value));
    }
    tags
}

#[test]
fn interpret_preserves_pyramid_order_properties_and_associated_images() {
    let mut first = tiled(1024, 512, 7);
    first.push(SyntheticTag::ascii(TAG_MAKE, "Huron LE176"));
    first.push(SyntheticTag::ascii(
        tags::IMAGE_DESCRIPTION,
        "Scanner = LE176\nObjective = 20\nValue = left=right\nEmpty = ",
    ));
    let file = build_tiff(&[
        first,
        stripped(100, 50, None),
        tiled(512, 256, 1),
        stripped(80, 40, Some(1)),
        stripped(120, 60, Some(9)),
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    let layout = HuronInterpreter.interpret(&container).unwrap();

    let levels = &layout.dataset.scenes[0].series[0].levels;
    assert_eq!(levels.len(), 2);
    assert_eq!(levels[0].dimensions, (1024, 512));
    assert_eq!(levels[1].dimensions, (512, 256));
    assert_eq!(levels[1].downsample, 2.0);
    assert_eq!(layout.dataset.properties.vendor(), Some("huron"));
    assert_eq!(
        layout.dataset.properties.get("huron.Scanner"),
        Some("LE176")
    );
    assert_eq!(
        layout.dataset.properties.get("huron.Value"),
        Some("left=right")
    );
    assert_eq!(layout.dataset.properties.get("huron.Empty"), Some(""));
    assert_eq!(
        layout.dataset.associated_images["thumbnail"].dimensions,
        (100, 50)
    );
    assert_eq!(
        layout.dataset.associated_images["label"].dimensions,
        (80, 40)
    );
    assert_eq!(
        layout.dataset.associated_images["macro"].dimensions,
        (120, 60)
    );
}

#[test]
fn image_depth_and_compression_validation_fail_closed() {
    let mut depth = tiled(64, 32, 7);
    depth.push(SyntheticTag::ascii(TAG_MAKE, "Huron"));
    depth.push(SyntheticTag::ascii(
        tags::IMAGE_DESCRIPTION,
        "Scanner=LE176",
    ));
    depth.push(SyntheticTag::long(TAG_IMAGE_DEPTH, 2));
    let file = build_tiff(&[depth]);
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(HuronInterpreter
        .interpret(&container)
        .unwrap_err()
        .to_string()
        .contains("ImageDepth=2"));

    let mut unsupported = tiled(64, 32, 99);
    unsupported.push(SyntheticTag::ascii(TAG_MAKE, "Huron"));
    unsupported.push(SyntheticTag::ascii(
        tags::IMAGE_DESCRIPTION,
        "Scanner=LE176",
    ));
    let file = build_tiff(&[unsupported]);
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(HuronInterpreter
        .interpret(&container)
        .unwrap_err()
        .to_string()
        .contains("unsupported TIFF compression"));
}

#[test]
fn missing_description_and_zero_tile_dimensions_are_rejected() {
    let mut missing = tiled(64, 32, 7);
    missing.push(SyntheticTag::ascii(TAG_MAKE, "Huron"));
    let file = build_tiff(&[missing]);
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(HuronInterpreter
        .interpret(&container)
        .unwrap_err()
        .to_string()
        .contains("ImageDescription"));

    let mut zero = tiled(64, 32, 7);
    zero.push(SyntheticTag::long(tags::TILE_WIDTH, 0));
    zero.push(SyntheticTag::ascii(TAG_MAKE, "Huron"));
    zero.push(SyntheticTag::ascii(
        tags::IMAGE_DESCRIPTION,
        "Scanner=LE176",
    ));
    let file = build_tiff(&[zero]);
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(HuronInterpreter
        .interpret(&container)
        .unwrap_err()
        .to_string()
        .contains("tile dimensions must be > 0"));
}
