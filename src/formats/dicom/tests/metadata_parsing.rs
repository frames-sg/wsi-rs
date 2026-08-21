use super::fixtures::*;
use super::*;

fn test_metadata_object() -> DefaultDicomObject {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata-helpers.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    parse_metadata_object_full(&path)
        .expect("parse generated metadata")
        .obj
}

#[test]
fn level0_properties_from_metadata_match_full_parse() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let path = workspace_root
        .join("downloads/openslide-testdata-extracted/dicom/dicom-cmu1-jp2k/DCM_0.dcm");
    if !path.is_file() {
        eprintln!(
            "skipping corpus-backed DICOM metadata test; missing {}",
            path.display()
        );
        return;
    }
    let meta = parse_metadata_object_full(&path).expect("full metadata parse");
    let properties_from_metadata = (
        optional_pixel_spacing_mpp(&meta.obj).unwrap_or(None),
        optional_f64_at(
            &meta.obj,
            (tags::OPTICAL_PATH_SEQUENCE, 0, tags::OBJECTIVE_LENS_POWER),
        )
        .unwrap_or(None),
    );
    assert_eq!(
        properties_from_metadata,
        parse_level0_properties(&path).expect("level0 property parse")
    );
}

#[test]
fn metadata_parse_rejects_oversized_declared_value_before_allocation() {
    const METADATA_ELEMENT_LIMIT: u32 = 16 * 1024 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oversized-metadata.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));

    let mut bytes = std::fs::read(&path).unwrap();
    let pixel_header = [0xE0, 0x7F, 0x10, 0x00, b'O', b'B', 0, 0];
    let pixel_offset = bytes
        .windows(pixel_header.len())
        .position(|candidate| candidate == pixel_header)
        .expect("test DICOM should contain explicit-VR Pixel Data");
    let mut hostile_header = vec![0x77, 0x77, 0x10, 0x00, b'O', b'B', 0, 0];
    hostile_header.extend_from_slice(&(METADATA_ELEMENT_LIMIT + 1).to_le_bytes());
    bytes.splice(pixel_offset..pixel_offset, hostile_header);
    std::fs::write(&path, bytes).unwrap();

    let error = match parse_metadata_object_full(&path) {
        Ok(_) => panic!("oversized metadata value must be rejected before allocation"),
        Err(error) => error,
    };

    assert!(
        error.to_string().contains("metadata element value limit"),
        "unexpected error: {error}"
    );
}

#[test]
fn top_level_pixel_spacing_is_mpp_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spacing.dcm");
    let mut options = TestDicomOptions::native(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    options.pixel_spacing = Some("0.0005\\0.00025");
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open DICOM slide");
    assert_eq!(
        slide.dataset().properties.get("openslide.mpp-x"),
        Some("0.25")
    );
    assert_eq!(
        slide.dataset().properties.get("openslide.mpp-y"),
        Some("0.5")
    );
}

#[test]
fn shared_functional_group_pixel_spacing_is_mpp_for_start_instance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared-spacing.dcm");
    let mut options = TestDicomOptions::native(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    options.pixel_spacing = None;
    options.shared_pixel_spacing = Some("0.0005\\0.00025");
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open DICOM slide");
    assert_eq!(
        slide.dataset().properties.get("openslide.mpp-x"),
        Some("0.25")
    );
    assert_eq!(
        slide.dataset().properties.get("openslide.mpp-y"),
        Some("0.5")
    );
}

#[test]
fn required_metadata_helpers_preserve_field_context() {
    let mut obj = test_metadata_object();
    let missing_tag = dicom_core::Tag(0x7777, 0x0010);
    let invalid_integer_tag = dicom_core::Tag(0x7777, 0x0011);
    let invalid_string_tag = dicom_core::Tag(0x7777, 0x0012);
    obj.put(DataElement::new(
        invalid_integer_tag,
        VR::LO,
        "not-an-integer",
    ));
    obj.put(DataElement::<InMemDicomObject>::new(
        invalid_string_tag,
        VR::SQ,
        DataSetSequence::from(vec![InMemDicomObject::new_empty()]),
    ));

    let missing_string = required_string(&obj, missing_tag, "MissingString").unwrap_err();
    assert!(missing_string.to_string().contains("missing MissingString"));
    let missing_integer = required_u32(&obj, missing_tag, "MissingInteger").unwrap_err();
    assert!(missing_integer
        .to_string()
        .contains("missing MissingInteger"));
    let invalid_integer = required_u32(&obj, invalid_integer_tag, "InvalidInteger").unwrap_err();
    assert!(invalid_integer
        .to_string()
        .contains("invalid InvalidInteger"));
    let invalid_string = required_string(&obj, invalid_string_tag, "InvalidString").unwrap_err();
    assert!(invalid_string.to_string().contains("invalid InvalidString"));

    let mut item = InMemDicomObject::new_empty();
    let missing_nested =
        required_u32_at_item(&item, missing_tag, "MissingNestedInteger").unwrap_err();
    assert!(missing_nested
        .to_string()
        .contains("missing MissingNestedInteger"));
    item.put(DataElement::new(
        invalid_integer_tag,
        VR::LO,
        "not-an-integer",
    ));
    let invalid_nested =
        required_u32_at_item(&item, invalid_integer_tag, "InvalidNestedInteger").unwrap_err();
    assert!(invalid_nested
        .to_string()
        .contains("invalid InvalidNestedInteger"));
}

#[test]
fn optional_metadata_helpers_reject_invalid_numeric_values() {
    let mut obj = test_metadata_object();
    let integer_tag = dicom_core::Tag(0x7777, 0x0020);
    let float_tag = dicom_core::Tag(0x7777, 0x0021);
    let pair_tag = dicom_core::Tag(0x7777, 0x0022);
    let string_tag = dicom_core::Tag(0x7777, 0x0023);
    obj.put(DataElement::new(integer_tag, VR::LO, "not-an-integer"));
    obj.put(DataElement::new(float_tag, VR::LO, "not-a-float"));
    obj.put(DataElement::new(pair_tag, VR::DS, "not-a-float\\2"));
    obj.put(DataElement::new(string_tag, VR::LO, "optional-value\0"));

    assert!(optional_u32(&obj, integer_tag)
        .unwrap_err()
        .to_string()
        .contains("invalid DICOM integer"));
    assert!(optional_f64_at(&obj, float_tag)
        .unwrap_err()
        .to_string()
        .contains("invalid DICOM float"));
    assert!(optional_pair_f64_at(&obj, pair_tag)
        .unwrap_err()
        .to_string()
        .contains("invalid DICOM float pair"));
    assert_eq!(
        optional_string(&obj, string_tag).expect("valid optional string"),
        Some("optional-value".into())
    );

    obj.put(DataElement::new(pair_tag, VR::DS, "1\\not-a-float"));
    assert!(optional_pair_f64_at(&obj, pair_tag)
        .unwrap_err()
        .to_string()
        .contains("invalid DICOM float pair"));

    obj.put(DataElement::<InMemDicomObject>::new(
        pair_tag,
        VR::SQ,
        DataSetSequence::from(vec![InMemDicomObject::new_empty()]),
    ));
    assert!(optional_pair_f64_at(&obj, pair_tag)
        .unwrap_err()
        .to_string()
        .contains("invalid DICOM string pair"));
}

#[test]
fn sparse_tile_map_validates_sequence_shape_and_one_based_positions() {
    let mut obj = test_metadata_object();
    let missing = parse_sparse_tile_map(&obj, 2, 2).unwrap_err();
    assert!(missing
        .to_string()
        .contains("missing PerFrameFunctionalGroupsSequence"));

    obj.put(DataElement::new(
        tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE,
        VR::LO,
        "not-a-sequence",
    ));
    let wrong_kind = parse_sparse_tile_map(&obj, 2, 2).unwrap_err();
    assert!(wrong_kind.to_string().contains("is not a sequence"));

    let mut position = InMemDicomObject::new_empty();
    position.put(DataElement::new(
        tags::COLUMN_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX,
        VR::UL,
        PrimitiveValue::from(3u32),
    ));
    position.put(DataElement::new(
        tags::ROW_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX,
        VR::UL,
        PrimitiveValue::from(1u32),
    ));
    let mut frame = InMemDicomObject::new_empty();
    frame.put(DataElement::<InMemDicomObject>::new(
        tags::PLANE_POSITION_SLIDE_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![position]),
    ));
    obj.put(DataElement::<InMemDicomObject>::new(
        tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![frame]),
    ));

    let map = parse_sparse_tile_map(&obj, 2, 2).expect("valid sparse frame position");
    assert_eq!(map.get(&(1, 0)), Some(&0));
}

#[test]
fn optical_path_profiles_require_sequence_and_byte_values() {
    let mut obj = test_metadata_object();
    obj.put(DataElement::new(
        tags::OPTICAL_PATH_SEQUENCE,
        VR::LO,
        "not-a-sequence",
    ));
    let wrong_sequence = optical_path_icc_profiles(&obj, "1.2.3").unwrap_err();
    assert!(wrong_sequence
        .to_string()
        .contains("OpticalPathSequence is not a sequence"));

    let mut optical_path = InMemDicomObject::new_empty();
    optical_path.put(DataElement::<InMemDicomObject>::new(
        tags::ICC_PROFILE,
        VR::SQ,
        DataSetSequence::from(vec![InMemDicomObject::new_empty()]),
    ));
    obj.put(DataElement::<InMemDicomObject>::new(
        tags::OPTICAL_PATH_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![optical_path]),
    ));
    let wrong_profile = optical_path_icc_profiles(&obj, "1.2.3").unwrap_err();
    assert!(wrong_profile
        .to_string()
        .contains("invalid DICOM OpticalPathSequence ICCProfile"));

    let mut optical_path = InMemDicomObject::new_empty();
    optical_path.put(DataElement::new(
        tags::ICC_PROFILE,
        VR::OB,
        PrimitiveValue::from(vec![1u8, 2, 3, 4]),
    ));
    optical_path.put(DataElement::<InMemDicomObject>::new(
        tags::OPTICAL_PATH_IDENTIFIER,
        VR::SQ,
        DataSetSequence::from(vec![InMemDicomObject::new_empty()]),
    ));
    obj.put(DataElement::<InMemDicomObject>::new(
        tags::OPTICAL_PATH_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![optical_path]),
    ));
    let wrong_identifier = optical_path_icc_profiles(&obj, "1.2.3").unwrap_err();
    assert!(wrong_identifier
        .to_string()
        .contains("invalid DICOM OpticalPathIdentifier"));
}

#[test]
fn metadata_parse_rejects_values_outside_u16_fields() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("valid.dcm");
    write_test_dicom(&source, TestDicomOptions::native(test_rgb_pixel_data()));

    for (tag, name) in [
        (tags::SAMPLES_PER_PIXEL, "SamplesPerPixel"),
        (tags::PLANAR_CONFIGURATION, "PlanarConfiguration"),
    ] {
        let mut object = dicom_object::open_file(&source).expect("open generated DICOM");
        object.put(DataElement::new(
            tag,
            VR::UL,
            PrimitiveValue::from(u32::from(u16::MAX) + 1),
        ));
        let path = dir.path().join(format!("out-of-range-{name}.dcm"));
        object.write_to_file(&path).expect("write hostile DICOM");

        let error = match parse_metadata_object_full(&path) {
            Ok(_) => panic!("out-of-range {name} should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("out of range"), "{error}");
        assert!(error.to_string().contains(name), "{error}");
    }
}

#[test]
fn metadata_identity_helpers_cover_equality_and_missing_path_fallback() {
    let path = Path::new("missing/metadata/fallback.dcm");
    assert_eq!(canonicalize_or_fallback(path), path);
    ensure_same_sop(path, "1.2.3", "1.2.3").expect("matching SOP instances");
    let mismatch = ensure_same_sop(path, "1.2.4", "1.2.3").unwrap_err();
    assert!(mismatch.to_string().contains("1.2.4 vs. 1.2.3"));
}
