use super::fixtures::{
    build_czi_bytes, metadata_xml, write_fixture, write_u64, AttachmentSpec, SubblockSpec,
};
use crate::core::registry::DatasetReader;
use crate::formats::zeiss::ZeissBackend;
use crate::WsiError;
use std::io::Write;

fn open_error(bytes: &[u8]) -> WsiError {
    let mut file = tempfile::Builder::new()
        .suffix(".czi")
        .tempfile()
        .expect("invalid CZI fixture");
    file.write_all(bytes).expect("write invalid CZI fixture");
    match ZeissBackend.open(file.path()) {
        Ok(_) => panic!("invalid CZI unexpectedly opened"),
        Err(error) => error,
    }
}

#[test]
fn open_reports_malformed_xml_and_missing_canvas_context() {
    let subblock = SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 2, 3]);
    let malformed = build_czi_bytes(std::slice::from_ref(&subblock), &[], "<ImageDocument");
    let error = open_error(&malformed);
    assert!(error.to_string().contains("XML") || error.to_string().contains("metadata"));

    let mut without_scene = subblock;
    without_scene.scene = None;
    let no_canvas = build_czi_bytes(&[without_scene], &[], "");
    let error = open_error(&no_canvas);
    assert!(error
        .to_string()
        .contains("missing Zeiss canvas dimensions"));
}

#[test]
fn open_rejects_empty_scene_metadata_and_oversized_decoded_subblocks() {
    let no_scenes_xml = "<ImageDocument><Metadata><Information><Image><SizeX>1</SizeX><SizeY>1</SizeY><SizeS>0</SizeS></Image></Information></Metadata></ImageDocument>";
    let mut without_scene = SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 2, 3]);
    without_scene.scene = None;
    let error = open_error(&build_czi_bytes(&[without_scene], &[], no_scenes_xml));
    assert!(error.to_string().contains("no scenes"));

    let mut oversized = SubblockSpec::bgr24(0, 0, 1, 1, Vec::new());
    oversized.stored_width = 50_000;
    oversized.stored_height = 50_000;
    oversized.pixel_type = 4;
    let error = open_error(&build_czi_bytes(&[oversized], &[], &metadata_xml(1, 1)));
    assert!(error.to_string().contains("decoded subblock"));
}

#[test]
fn open_rejects_attachment_resource_limits_and_truncated_payload_ranges() {
    let subblock = SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 2, 3]);
    let attachments: Vec<_> = (0..1_025)
        .map(|_| AttachmentSpec {
            name: "Ignored",
            file_type: "TXT",
            data: Vec::new(),
        })
        .collect();
    let error = open_error(&build_czi_bytes(
        std::slice::from_ref(&subblock),
        &attachments,
        &metadata_xml(1, 1),
    ));
    assert!(error.to_string().contains("attachment count"));

    let attachment = AttachmentSpec {
        name: "Ignored",
        file_type: "TXT",
        data: vec![1; 20],
    };
    let mut truncated = build_czi_bytes(
        std::slice::from_ref(&subblock),
        std::slice::from_ref(&attachment),
        &metadata_xml(1, 1),
    );
    truncated.truncate(truncated.len() - 10);
    let error = open_error(&truncated);
    assert!(error.to_string().contains("beyond file length"));

    let mut oversized = build_czi_bytes(&[subblock], &[attachment], &metadata_xml(1, 1));
    let attachment_segment = oversized.len() - (32 + 256 + 20);
    write_u64(
        &mut oversized,
        attachment_segment + 32,
        crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 1,
    );
    let error = open_error(&oversized);
    assert!(error.to_string().contains("safety limit"));
}

#[test]
fn open_rejects_bad_jpeg_attachment_after_dimension_probe_fallback() {
    let fixture = write_fixture(
        &[SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 2, 3])],
        &[AttachmentSpec {
            name: "Thumbnail",
            file_type: "JPG",
            data: b"not-a-jpeg".to_vec(),
        }],
        &metadata_xml(1, 1),
    );
    let error = match ZeissBackend.open(fixture.path()) {
        Ok(_) => panic!("invalid associated JPEG unexpectedly opened"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("JPEG") || error.to_string().contains("jpeg"));
}
