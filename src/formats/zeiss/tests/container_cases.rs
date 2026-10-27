use super::fixtures::{build_czi_bytes, main_fixture, metadata_xml, AttachmentSpec, SubblockSpec};
use crate::formats::zeiss::container::{CziContainer, Dimension, PixelType};
use std::io::Write;

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
