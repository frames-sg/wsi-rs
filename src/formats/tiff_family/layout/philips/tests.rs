use super::*;
use crate::formats::tiff_family::test_support::{build_tiff, SyntheticTag};

fn philips_description() -> &'static str {
    r#"<DataObject ObjectType="DPUfsImport">
        <Attribute Name="DICOM_PIXEL_SPACING">"0.000226891" "0.000226907"</Attribute>
        <Attribute Name="DICOM_ACQUISITION_DATETIME">20200101120000</Attribute>
        <Attribute Name="PIM_DP_SCANNER_OPERATOR_ID">user@example.com</Attribute>
        <Attribute Name="EmptyAttr"></Attribute>
        <Attribute Name="PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE">
            <DataObject ObjectType="PixelDataRepresentation">
                <Attribute Name="DICOM_PIXEL_SPACING">0.000227273 0.000227273</Attribute>
            </DataObject>
            <DataObject ObjectType="PixelDataRepresentation">
                <Attribute Name="DICOM_PIXEL_SPACING">0.000454545 0.000454545</Attribute>
            </DataObject>
        </Attribute>
    </DataObject>"#
}

#[test]
fn interpret_builds_corrected_pyramid_properties_and_associated_images() {
    let file = build_tiff(&[
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 1024),
            SyntheticTag::long(tags::IMAGE_LENGTH, 512),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(TAG_SOFTWARE, "Philips DPUfsImport"),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, philips_description()),
        ],
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 510),
            SyntheticTag::long(tags::IMAGE_LENGTH, 254),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 33004),
        ],
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 120),
            SyntheticTag::long(tags::IMAGE_LENGTH, 80),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::long(tags::STRIP_OFFSETS, 0),
            SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 0),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Philips label image"),
        ],
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = PhilipsInterpreter;

    assert!(interpreter.detect(&container));
    assert_eq!(interpreter.vendor_name(), "philips");
    let layout = interpreter.interpret(&container).unwrap();

    let levels = &layout.dataset.scenes[0].series[0].levels;
    assert_eq!(levels.len(), 2);
    assert_eq!(levels[0].dimensions, (1024, 512));
    assert_eq!(levels[1].dimensions, (512, 256));
    assert_eq!(levels[1].downsample, 2.0);
    assert!(layout.dataset.associated_images.contains_key("label"));
    assert!(layout.associated_sources.contains_key("label"));
    assert_eq!(
        layout.dataset.properties.get("openslide.mpp-x"),
        Some("0.226907")
    );
    assert_eq!(
        layout.dataset.properties.get("openslide.mpp-y"),
        Some("0.226891")
    );
    assert_eq!(
        layout
            .dataset
            .properties
            .get("philips.DICOM_ACQUISITION_DATETIME"),
        Some("20200101120000")
    );
    assert_eq!(layout.tile_sources.len(), 2);
}

#[test]
fn interpret_rejects_zero_tiles_and_missing_pyramid() {
    let zero_tile = build_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 64),
        SyntheticTag::long(tags::IMAGE_LENGTH, 64),
        SyntheticTag::long(tags::TILE_WIDTH, 0),
        SyntheticTag::long(tags::TILE_LENGTH, 32),
        SyntheticTag::ascii(TAG_SOFTWARE, "Philips"),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, philips_description()),
    ]]);
    let container = TiffContainer::open(zero_tile.path()).unwrap();
    assert!(PhilipsInterpreter
        .interpret(&container)
        .unwrap_err()
        .to_string()
        .contains("tile dimensions must be > 0"));

    let stripped = build_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 64),
        SyntheticTag::long(tags::IMAGE_LENGTH, 64),
        SyntheticTag::ascii(TAG_SOFTWARE, "Philips"),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, philips_description()),
    ]]);
    let container = TiffContainer::open(stripped.path()).unwrap();
    assert!(PhilipsInterpreter
        .interpret(&container)
        .unwrap_err()
        .to_string()
        .contains("No tiled pyramid levels"));
}

#[test]
fn detect_and_associated_classification_cover_negative_cases() {
    let empty = build_tiff(&[vec![]]);
    assert!(!PhilipsInterpreter.detect(&TiffContainer::open(empty.path()).unwrap()));

    let file = build_tiff(&[
        vec![
            SyntheticTag::ascii(TAG_SOFTWARE, "Other"),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, philips_description()),
        ],
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 8),
            SyntheticTag::long(tags::IMAGE_LENGTH, 8),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "MACRO overview"),
        ],
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 8),
            SyntheticTag::long(tags::IMAGE_LENGTH, 8),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "unclassified"),
        ],
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(!PhilipsInterpreter.detect(&container));
    assert_eq!(
        classify_associated(&container, container.top_ifds()[1]).as_deref(),
        Some("macro")
    );
    assert_eq!(
        classify_associated(&container, container.top_ifds()[2]),
        None
    );
}

#[test]
fn compression_from_tag_values() {
    assert_eq!(compression_from_tag(1), Compression::None);
    assert_eq!(compression_from_tag(6), Compression::Jpeg);
    assert_eq!(compression_from_tag(7), Compression::Jpeg);
    assert_eq!(compression_from_tag(33003), Compression::Jp2kYcbcr);
    assert_eq!(compression_from_tag(33004), Compression::Jp2kRgb);
    assert_eq!(compression_from_tag(33005), Compression::Jp2kYcbcr);
    assert_eq!(compression_from_tag(99), Compression::Other(99));
    assert_eq!(compression_from_tag(50000), Compression::Zstd);
}

#[test]
fn parse_spacing_accepts_valid_and_rejects_invalid_values() {
    assert_eq!(parse_spacing("0.000243 0.000243"), Some(0.000243));
    assert_eq!(parse_spacing("  0.5 0.5  "), Some(0.5));
    assert_eq!(parse_spacing("0.001"), Some(0.001));
    assert_eq!(parse_spacing("\"0.000243\" \"0.000250\""), Some(0.000243));
    for invalid in ["", "   ", "abc", "0.0 0.0", "-1.0 -1.0"] {
        assert_eq!(parse_spacing(invalid), None);
    }
}

#[test]
fn parse_spacing_pair_preserves_distinct_axes() {
    assert_eq!(
        parse_spacing_pair("\"0.000226891\" \"0.000226907\""),
        Some((0.000226891, 0.000226907))
    );
    assert_eq!(parse_spacing_pair("0.001"), Some((0.001, 0.001)));
}

#[test]
fn collect_pixel_spacings_recurses_and_handles_empty_xml() {
    let root = xml::parse_xml(philips_description()).unwrap();
    let mut spacings = Vec::new();
    collect_pixel_spacings(&root, &mut spacings);
    assert_eq!(spacings, vec![0.000226891, 0.000227273, 0.000454545]);

    let root = xml::parse_xml(
        r#"<DataObject ObjectType="DPUfsImport"><Attribute Name="Other">value</Attribute></DataObject>"#,
    )
    .unwrap();
    spacings.clear();
    collect_pixel_spacings(&root, &mut spacings);
    assert!(spacings.is_empty());
}

#[test]
fn extraction_and_property_helpers_preserve_metadata() {
    let root = xml::parse_xml(philips_description()).unwrap();
    assert_eq!(
        extract_representation_spacings(&root),
        Some(vec![0.000227273, 0.000454545])
    );
    assert_eq!(
        find_first_pixel_spacing(&root),
        Some("\"0.000226891\" \"0.000226907\"")
    );
    let mut props = Properties::new();
    collect_xml_properties(&root, &mut props);
    assert_eq!(
        props.get("philips.DICOM_ACQUISITION_DATETIME"),
        Some("20200101120000")
    );
    assert_eq!(
        props.get("philips.PIM_DP_SCANNER_OPERATOR_ID"),
        Some("user@example.com")
    );
    assert_eq!(props.get("philips.EmptyAttr"), None);
    assert_eq!(
        resolve_mpp_pair(Some("\"0.000226891\" \"0.000226907\""), Some(0.123)),
        Some((0.226907, 0.226891))
    );
    assert_eq!(resolve_mpp_pair(None, Some(0.000243)), Some((0.243, 0.243)));
}

#[test]
fn public_level_dimensions_follow_exact_power_of_two_chain() {
    assert_eq!(
        philips_public_level_dimensions((45056, 35840), 0),
        (45056, 35840)
    );
    assert_eq!(
        philips_public_level_dimensions((45056, 35840), 2),
        (11264, 8960)
    );
    assert_eq!(
        philips_public_level_dimensions((131072, 100352), 8),
        (512, 392)
    );
    assert_eq!(100352_u64.div_ceil(512), 196);
}
