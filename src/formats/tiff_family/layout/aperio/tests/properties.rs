use super::super::*;
use super::fixtures::{build_aperio_tiff, SyntheticTag};
// ── Property parsing tests ───────────────────────────────────────

#[test]
fn properties_vendor_and_comment() {
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::ascii(
            tags::IMAGE_DESCRIPTION,
            "Aperio Image Library v12.0.15|AppMag = 40|MPP = 0.2528",
        ),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert_eq!(layout.dataset.properties.vendor(), Some("aperio"));
    assert_eq!(
        layout.dataset.properties.get("openslide.comment"),
        Some("Aperio Image Library v12.0.15|AppMag = 40|MPP = 0.2528"),
    );
}

#[test]
fn properties_aperio_keys_parsed() {
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::ascii(
            tags::IMAGE_DESCRIPTION,
            "Aperio Image Library v12.0.15|AppMag = 40|MPP = 0.2528|StripeWidth = 1000",
        ),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert_eq!(layout.dataset.properties.get("aperio.AppMag"), Some("40"));
    assert_eq!(layout.dataset.properties.get("aperio.MPP"), Some("0.2528"));
    assert_eq!(
        layout.dataset.properties.get("aperio.StripeWidth"),
        Some("1000"),
    );
}

#[test]
fn properties_objective_power_and_mpp() {
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::ascii(
            tags::IMAGE_DESCRIPTION,
            "Aperio Image Library v12.0.15|AppMag = 40|MPP = 0.2528",
        ),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert_eq!(
        layout.dataset.properties.get("openslide.objective-power"),
        Some("40"),
    );
    assert_eq!(
        layout.dataset.properties.get("openslide.mpp-x"),
        Some("0.2528"),
    );
    assert_eq!(
        layout.dataset.properties.get("openslide.mpp-y"),
        Some("0.2528"),
    );

    // Verify via convenience accessors
    assert!((layout.dataset.properties.objective_power().unwrap() - 40.0).abs() < 0.001);
    let (mpp_x, mpp_y) = layout.dataset.properties.mpp().unwrap();
    assert!((mpp_x - 0.2528).abs() < 0.0001);
    assert!((mpp_y - 0.2528).abs() < 0.0001);
}
