use super::fixtures::main_fixture;
use crate::formats::zeiss::attachments::{
    associated_name, decode_associated_attachment, guid_bytes, probe_associated_attachment,
};

#[test]
fn associated_names_match_only_the_three_supported_zeiss_roles() {
    assert_eq!(associated_name("Label"), Some("label"));
    assert_eq!(associated_name("SlidePreview"), Some("macro"));
    assert_eq!(associated_name("Thumbnail"), Some("thumbnail"));
    for unsupported in ["label", "Preview", "", "Ignored"] {
        assert_eq!(associated_name(unsupported), None);
    }
}

#[test]
fn guid_bytes_follow_czi_field_endianness_and_reject_malformed_values() {
    assert_eq!(
        guid_bytes("00112233-4455-6677-8899-aabbccddeeff").expect("valid GUID"),
        [
            0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]
    );
    let shape_error = guid_bytes("not-a-guid").expect_err("invalid GUID shape");
    assert!(shape_error
        .to_string()
        .contains("unexpected Zeiss GUID format"));
    let hex_error =
        guid_bytes("0011223x-4455-6677-8899-aabbccddeeff").expect_err("invalid GUID hex");
    assert!(hex_error.to_string().contains("invalid GUID hex"));
}

#[test]
fn unknown_attachments_are_ignored_and_jpeg_probe_falls_back_to_decode() {
    let fixture = main_fixture();
    let mut czi = czi_rs::CziFile::open(fixture.path()).expect("open fixture with attachments");
    let unknown = czi
        .attachments()
        .iter()
        .find(|attachment| attachment.name == "Ignored")
        .expect("unknown attachment")
        .clone();
    assert!(decode_associated_attachment(&mut czi, &unknown)
        .expect("ignore unknown attachment")
        .is_none());

    let label = czi
        .attachments()
        .iter()
        .find(|attachment| attachment.name == "Label")
        .expect("label attachment")
        .clone();
    let metadata =
        probe_associated_attachment(std::path::Path::new("missing-source.czi"), &mut czi, &label)
            .expect("fallback label decode")
            .expect("supported label metadata");
    assert_eq!(metadata.dimensions, (2, 1));
    assert_eq!(metadata.channels, 3);
}
