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
    assert!(
        decode_associated_attachment(&mut czi, &unknown, crate::SlideLimits::default())
            .expect("ignore unknown attachment")
            .is_none()
    );

    let label = czi
        .attachments()
        .iter()
        .find(|attachment| attachment.name == "Label")
        .expect("label attachment")
        .clone();
    let metadata = probe_associated_attachment(
        std::path::Path::new("missing-source.czi"),
        &mut czi,
        &label,
        crate::SlideLimits::default(),
    )
    .expect("fallback label decode")
    .expect("supported label metadata");
    assert_eq!(metadata.dimensions, (2, 1));
    assert_eq!(metadata.channels, 3);
}

#[test]
fn embedded_attachment_probe_preserves_typed_metadata_without_composing_pixels() {
    use super::fixtures::{
        build_czi_bytes, metadata_xml, write_fixture, AttachmentSpec, SubblockSpec,
    };
    use crate::formats::zeiss::attachments::EMBEDDED_COMPOSED_PIXELS;

    for (pixel_type, bytes_per_pixel) in [(3, 3), (4, 6), (9, 4)] {
        let data = (1..=2 * bytes_per_pixel).map(|value| value as u8).collect();
        let mut source = SubblockSpec::bgr24(1, 2, 2, 1, data);
        source.pixel_type = pixel_type;
        let embedded = build_czi_bytes(&[source], &[], &metadata_xml(2, 1));
        let fixture = write_fixture(
            &[SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 2, 3])],
            &[AttachmentSpec {
                name: "SlidePreview",
                file_type: "CZI",
                data: embedded,
            }],
            &metadata_xml(1, 1),
        );
        let mut czi = czi_rs::CziFile::open(fixture.path()).unwrap();
        let attachment = czi.attachments()[0].clone();
        let (expected, pixels) =
            decode_associated_attachment(&mut czi, &attachment, crate::SlideLimits::default())
                .unwrap()
                .unwrap();
        match pixel_type {
            3 => assert_eq!(pixels.as_u8().unwrap(), &[3, 2, 1, 6, 5, 4]),
            4 => assert_eq!(
                pixels.data.as_u16().unwrap(),
                &[0x0605, 0x0403, 0x0201, 0x0c0b, 0x0a09, 0x0807]
            ),
            9 => assert_eq!(pixels.as_u8().unwrap(), &[3, 2, 1, 7, 6, 5]),
            _ => unreachable!(),
        }
        EMBEDDED_COMPOSED_PIXELS.with(|count| count.set(0));
        let actual = probe_associated_attachment(
            fixture.path(),
            &mut czi,
            &attachment,
            crate::SlideLimits::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(actual.dimensions, expected.dimensions);
        assert_eq!(actual.sample_type, expected.sample_type);
        assert_eq!(actual.channels, expected.channels);
        assert_eq!(actual.icc_profile, expected.icc_profile);
        EMBEDDED_COMPOSED_PIXELS.with(|count| {
            assert_eq!(
                count.get(),
                0,
                "metadata probing must not compose an output plane"
            )
        });
    }
}

#[test]
fn embedded_attachment_probe_keeps_payload_and_pixel_contract_errors() {
    use super::fixtures::{
        build_czi_bytes, metadata_xml, write_fixture, AttachmentSpec, SubblockSpec,
    };

    let short = SubblockSpec::bgr24(0, 0, 2, 1, vec![1, 2]);
    let mut unsupported = SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 2, 3]);
    unsupported.compression = 5;
    let mut typed = SubblockSpec::bgr24(1, 0, 1, 1, vec![0; 6]);
    typed.pixel_type = 4;
    for sources in [
        vec![short],
        vec![unsupported],
        vec![SubblockSpec::bgr24(0, 0, 1, 1, vec![0; 3]), typed],
    ] {
        let embedded = build_czi_bytes(&sources, &[], &metadata_xml(2, 1));
        let fixture = write_fixture(
            &[SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 2, 3])],
            &[AttachmentSpec {
                name: "SlidePreview",
                file_type: "CZI",
                data: embedded,
            }],
            &metadata_xml(1, 1),
        );
        let mut czi = czi_rs::CziFile::open(fixture.path()).unwrap();
        let attachment = czi.attachments()[0].clone();
        let expected =
            decode_associated_attachment(&mut czi, &attachment, crate::SlideLimits::default())
                .unwrap_err();
        let actual = probe_associated_attachment(
            fixture.path(),
            &mut czi,
            &attachment,
            crate::SlideLimits::default(),
        )
        .unwrap_err();
        assert_eq!(actual.to_string(), expected.to_string());
    }
}
