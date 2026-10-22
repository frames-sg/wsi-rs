use super::fixtures::{build_czi_bytes, main_fixture, metadata_xml, AttachmentSpec, SubblockSpec};
use crate::formats::zeiss::container::{CziContainer, Dimension, PixelType};
use std::io::Write;
use std::path::Path;

#[test]
fn native_container_reads_directories_metadata_subblocks_and_attachments() {
    let fixture = main_fixture();
    let mut container = CziContainer::open(fixture.path()).expect("open native CZI container");

    assert_eq!(container.file_header().major, 1);
    assert_eq!(container.subblocks().len(), 3);
    assert_eq!(container.subblocks()[0].pixel_type, PixelType::Bgr24);
    assert_eq!(
        container.subblocks()[0].coordinate.get(Dimension::S),
        Some(0)
    );
    assert_eq!(container.statistics().subblock_count, 3);
    assert_eq!(container.statistics().bounding_box_layer0.unwrap().w, 4);

    let xml = container.metadata_xml().expect("read metadata XML");
    assert!(xml.contains("Fixture Writer"));
    let summary = container.metadata().expect("parse metadata summary");
    assert_eq!(summary.image.sizes.get(&Dimension::X), Some(&4));
    assert_eq!(summary.channels[0].name.as_deref(), Some("Brightfield"));

    let raw = container.read_subblock(0).expect("read raw subblock");
    assert_eq!(raw.data, [3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10]);
    let label = container
        .attachments()
        .iter()
        .find(|attachment| attachment.name == "Label")
        .expect("label attachment")
        .clone();
    let blob = container
        .read_attachment(label.index)
        .expect("read attachment payload");
    assert_eq!(blob.info, label);
    assert!(blob.data.starts_with(&[0xff, 0xd8]));
}

#[test]
fn native_container_rejects_directory_geometry_that_overflows_signed_coordinates() {
    let bytes = build_czi_bytes(
        &[SubblockSpec::bgr24(i32::MAX, 0, 1, 1, vec![1, 2, 3])],
        &[AttachmentSpec {
            name: "Ignored",
            file_type: "TXT",
            data: Vec::new(),
        }],
        &metadata_xml(1, 1),
    );
    let mut fixture = tempfile::NamedTempFile::new().expect("create malformed CZI");
    fixture.write_all(&bytes).expect("write malformed CZI");
    let error = CziContainer::open(fixture.path()).expect_err("reject overflowing extent");
    assert!(error.to_string().contains("geometry"), "{error}");
}

#[test]
fn native_container_matches_the_dev_oracle_on_generated_and_jpegxr_corpus_files() {
    fn compare(path: &Path) {
        let mut native = CziContainer::open(path).expect("open native container");
        let mut oracle = czi_rs::CziFile::open(path).expect("open CZI oracle");

        assert_eq!(
            native.file_header().primary_file_guid,
            oracle.file_header().primary_file_guid
        );
        assert_eq!(
            native.file_header().file_guid,
            oracle.file_header().file_guid
        );
        assert_eq!(native.subblocks().len(), oracle.subblocks().len());
        for (actual, expected) in native.subblocks().iter().zip(oracle.subblocks()) {
            assert_eq!(actual.file_position, expected.file_position);
            assert_eq!(actual.file_part, expected.file_part);
            assert_eq!(actual.pixel_type.as_str(), expected.pixel_type.as_str());
            assert_eq!(actual.compression.as_str(), expected.compression.as_str());
            assert_eq!(
                (actual.rect.x, actual.rect.y, actual.rect.w, actual.rect.h),
                (
                    expected.rect.x,
                    expected.rect.y,
                    expected.rect.w,
                    expected.rect.h
                )
            );
            assert_eq!(
                (actual.stored_size.w, actual.stored_size.h),
                (expected.stored_size.w, expected.stored_size.h)
            );
            for dimension in Dimension::FRAME_ORDER {
                let oracle_dimension = czi_rs::Dimension::from_code(dimension.as_str()).unwrap();
                assert_eq!(
                    actual.coordinate.get(dimension),
                    expected.coordinate.get(oracle_dimension)
                );
            }
        }
        assert_eq!(native.attachments().len(), oracle.attachments().len());
        for (actual, expected) in native.attachments().iter().zip(oracle.attachments()) {
            assert_eq!(actual.file_position, expected.file_position);
            assert_eq!(actual.content_file_type, expected.content_file_type);
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.data_size, expected.data_size);
        }

        let actual = native.metadata().expect("native metadata");
        let expected = oracle.metadata().expect("oracle metadata");
        for dimension in [Dimension::X, Dimension::Y, Dimension::S] {
            let oracle_dimension = czi_rs::Dimension::from_code(dimension.as_str()).unwrap();
            assert_eq!(
                actual.image.sizes.get(&dimension),
                expected.image.sizes.get(&oracle_dimension)
            );
        }
        assert_eq!(actual.document.user_name, expected.document.user_name);
        assert_eq!(
            actual.document.creation_date,
            expected.document.creation_date
        );
        assert_eq!(actual.scaling.x, expected.scaling.x);
        assert_eq!(actual.scaling.y, expected.scaling.y);
        assert_eq!(actual.channels.len(), expected.channels.len());
        for (actual, expected) in actual.channels.iter().zip(&expected.channels) {
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.color, expected.color);
        }
    }

    let generated = main_fixture();
    compare(generated.path());
    compare(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jxr/rgb.czi"));
}
