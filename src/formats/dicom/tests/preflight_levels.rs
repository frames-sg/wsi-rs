use super::fixtures::*;
use super::manifest_building::{series_level_dimensions, write_series_level};
use super::runtime::*;
use super::*;
fn test_dicom_image(sop_instance_uid: &str, grid: DicomGrid) -> Arc<DicomImage> {
    test_dicom_image_with_transfer_syntax(sop_instance_uid, grid, uids::EXPLICIT_VR_LITTLE_ENDIAN)
}
#[test]
fn rejects_huge_single_level_regular_dicom_missing_physical_pyramid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge-base-only.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.rows = 512;
    options.columns = 512;
    options.total_pixel_matrix_rows = 32_768;
    options.total_pixel_matrix_columns = 32_768;
    options.number_of_frames = 4_096;
    write_test_dicom(&path, options);

    let err = Slide::open(&path).expect_err("huge base-only DICOM should fail fast");
    let message = err.to_string();
    assert!(
        message.contains("contains only a full-resolution base layer"),
        "unexpected error: {message}"
    );
    assert!(
        message.contains("Open the complete DICOM series/folder"),
        "unexpected error: {message}"
    );
}

#[test]
fn small_single_level_dicom_remains_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small-single-level.dcm");
    write_series_level(&path, "1.2.826.0.1.3680043.10.777.1", 16, 16);

    let slide = Slide::open(&path).expect("small single-level DICOM remains supported");
    assert_eq!(series_level_dimensions(&slide), vec![(16, 16)]);
}

#[test]
fn build_levels_groups_split_sparse_instances() {
    let mut first_tiles = HashMap::new();
    first_tiles.insert((0, 0), 0);
    let mut second_tiles = HashMap::new();
    second_tiles.insert((1, 0), 0);

    let levels = build_levels(
        Path::new("split.dcm"),
        vec![
            test_dicom_image("1.2.3.1", DicomGrid::Sparse(first_tiles)),
            test_dicom_image("1.2.3.2", DicomGrid::Sparse(second_tiles)),
        ],
    )
    .expect("split sparse parts should form one logical level");

    assert_eq!(levels.len(), 1);
    assert_eq!(levels[0].parts.len(), 2);
    assert_eq!(levels[0].tiles_across, 8);
    assert_eq!(levels[0].tiles_down, 8);
}

#[test]
fn tile_codec_kind_uses_actual_sparse_split_part_for_request() {
    let mut first_tiles = HashMap::new();
    first_tiles.insert((0, 0), 0);
    let mut second_tiles = HashMap::new();
    second_tiles.insert((1, 0), 0);

    let levels = build_levels(
        Path::new("split-codec.dcm"),
        vec![
            test_dicom_image_with_transfer_syntax(
                "1.2.3.1",
                DicomGrid::Sparse(first_tiles),
                JPEG_TRANSFER_SYNTAX,
            ),
            test_dicom_image_with_transfer_syntax(
                "1.2.3.2",
                DicomGrid::Sparse(second_tiles),
                HTJ2K_LOSSLESS_TRANSFER_SYNTAX,
            ),
        ],
    )
    .expect("split sparse parts should form one logical level");
    let reader = DicomReader {
        slide: Arc::new(DicomSlide {
            dataset: empty_dataset(),
            levels,
            associated: HashMap::new(),
        }),
    };

    assert_eq!(
        reader.tile_codec_kind(&tile_request(0, 0)),
        TileCodecKind::Jpeg
    );
    assert_eq!(
        reader.tile_codec_kind(&tile_request(1, 0)),
        TileCodecKind::Htj2k
    );
    assert_eq!(
        reader.tile_codec_kind(&tile_request(2, 0)),
        TileCodecKind::Other
    );
}
