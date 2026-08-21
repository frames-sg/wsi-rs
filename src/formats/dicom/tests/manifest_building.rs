use super::fixtures::*;
use super::*;
fn icc_key(scene: usize, series: usize) -> IccProfileKey {
    IccProfileKey::new(SceneId::new(scene), SeriesId::new(series))
}

fn test_optical_path_icc(bytes: Vec<u8>) -> TestOpticalPathIccProfile {
    TestOpticalPathIccProfile {
        optical_path_identifier: None,
        bytes,
    }
}

fn test_optical_path_icc_with_identifier(
    optical_path_identifier: &'static str,
    bytes: Vec<u8>,
) -> TestOpticalPathIccProfile {
    TestOpticalPathIccProfile {
        optical_path_identifier: Some(optical_path_identifier),
        bytes,
    }
}

fn write_optical_path_icc_instance(
    path: &Path,
    sop_instance_uid: &'static str,
    optical_path_icc_profiles: Vec<TestOpticalPathIccProfile>,
) {
    let mut options = TestDicomOptions::native(test_rgb_pixel_data());
    options.sop_instance_uid = sop_instance_uid;
    options.optical_path_icc_profiles = optical_path_icc_profiles;
    write_test_dicom(path, options);
}

fn write_two_optical_path_icc_instances(
    dir: &Path,
    first_profiles: Vec<TestOpticalPathIccProfile>,
    second_profiles: Vec<TestOpticalPathIccProfile>,
) {
    write_optical_path_icc_instance(
        &dir.join("first.dcm"),
        "1.2.826.0.1.3680043.10.777.1",
        first_profiles,
    );
    write_optical_path_icc_instance(
        &dir.join("second.dcm"),
        "1.2.826.0.1.3680043.10.777.2",
        second_profiles,
    );
}

fn assert_two_optical_path_source_icc_profiles(dataset: &Dataset) {
    assert!(!dataset.icc_profiles.contains_key(&icc_key(0, 0)));
    assert_eq!(dataset.source_icc_profiles.len(), 2);
    assert_eq!(dataset.source_icc_profiles[0].key.optical_path, Some(0));
    assert_eq!(dataset.source_icc_profiles[0].bytes, vec![1, 2, 3, 4]);
    assert_eq!(dataset.source_icc_profiles[1].key.optical_path, Some(1));
    assert_eq!(dataset.source_icc_profiles[1].bytes, vec![5, 8, 13, 21]);
}

#[test]
fn dicom_manifest_preserves_optical_path_icc_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("optical-path-icc.dcm");
    let icc_bytes = vec![0, 1, 2, 3, 5, 8, 13, 21];
    let mut options = TestDicomOptions::native(test_rgb_pixel_data());
    options.optical_path_icc_profiles = vec![test_optical_path_icc(icc_bytes.clone())];
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open DICOM slide");
    let dataset = slide.dataset();
    assert_eq!(dataset.source_icc_profiles.len(), 1);
    let profile = &dataset.source_icc_profiles[0];
    assert_eq!(profile.key.scene, SceneId::new(0));
    assert_eq!(profile.key.series, SeriesId::new(0));
    assert_eq!(profile.key.optical_path, None);
    assert_eq!(profile.key.channel, None);
    assert_eq!(profile.bytes, icc_bytes);
    assert_eq!(dataset.icc_profiles.get(&icc_key(0, 0)), Some(&icc_bytes));
    match &profile.provenance {
        IccProfileProvenance::DicomOpticalPath {
            sop_instance_uid,
            optical_path_identifier,
            ..
        } => {
            assert_eq!(sop_instance_uid, "1.2.826.0.1.3680043.10.777.1");
            assert_eq!(optical_path_identifier, &None);
        }
        other => panic!("unexpected ICC provenance: {other:?}"),
    }
}

#[test]
fn dicom_manifest_collapses_identical_optical_path_icc_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("identical-optical-path-icc.dcm");
    let icc_bytes = vec![3, 1, 4, 1, 5, 9];
    let mut options = TestDicomOptions::native(test_rgb_pixel_data());
    options.optical_path_icc_profiles = vec![
        test_optical_path_icc_with_identifier("brightfield", icc_bytes.clone()),
        test_optical_path_icc_with_identifier("duplicate", icc_bytes.clone()),
    ];
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open DICOM slide");
    let dataset = slide.dataset();
    assert_eq!(dataset.source_icc_profiles.len(), 1);
    let profile = &dataset.source_icc_profiles[0];
    assert_eq!(profile.key.optical_path, None);
    assert_eq!(profile.bytes, icc_bytes);
    assert_eq!(dataset.icc_profiles.get(&icc_key(0, 0)), Some(&icc_bytes));
    match &profile.provenance {
        IccProfileProvenance::DicomOpticalPath {
            optical_path_identifier,
            ..
        } => assert_eq!(optical_path_identifier.as_deref(), Some("brightfield")),
        other => panic!("unexpected ICC provenance: {other:?}"),
    }
}

#[test]
fn dicom_manifest_preserves_different_optical_path_icc_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("different-optical-path-icc.dcm");
    let mut options = TestDicomOptions::native(test_rgb_pixel_data());
    options.optical_path_icc_profiles = vec![
        test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
        test_optical_path_icc_with_identifier("path-b", vec![1, 2, 4, 5]),
    ];
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open DICOM slide");
    let dataset = slide.dataset();
    assert!(!dataset.icc_profiles.contains_key(&icc_key(0, 0)));
    assert_eq!(dataset.source_icc_profiles.len(), 2);
    assert_eq!(dataset.source_icc_profiles[0].key.optical_path, Some(0));
    assert_eq!(dataset.source_icc_profiles[0].bytes, vec![1, 2, 3, 4]);
    assert_eq!(dataset.source_icc_profiles[1].key.optical_path, Some(1));
    assert_eq!(dataset.source_icc_profiles[1].bytes, vec![1, 2, 4, 5]);
    match &dataset.source_icc_profiles[1].provenance {
        IccProfileProvenance::DicomOpticalPath {
            optical_path_identifier,
            ..
        } => assert_eq!(optical_path_identifier.as_deref(), Some("path-b")),
        other => panic!("unexpected ICC provenance: {other:?}"),
    }
}

#[test]
fn dicom_manifest_accepts_identical_icc_profiles_across_volume_instances() {
    let dir = tempfile::tempdir().unwrap();
    let icc_bytes = vec![2, 7, 1, 8, 2, 8];
    let first_path = dir.path().join("first.dcm");
    let mut first_options = TestDicomOptions::native(test_rgb_pixel_data());
    first_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.1";
    first_options.optical_path_icc_profiles = vec![test_optical_path_icc(icc_bytes.clone())];
    write_test_dicom(&first_path, first_options);
    let second_path = dir.path().join("second.dcm");
    let mut second_options = TestDicomOptions::native(test_rgb_pixel_data());
    second_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.2";
    second_options.optical_path_icc_profiles = vec![test_optical_path_icc(icc_bytes.clone())];
    write_test_dicom(&second_path, second_options);

    let slide = Slide::open(dir.path()).expect("open DICOM directory");
    let dataset = slide.dataset();
    assert_eq!(dataset.source_icc_profiles.len(), 1);
    assert_eq!(dataset.source_icc_profiles[0].bytes, icc_bytes);
    assert_eq!(dataset.icc_profiles.get(&icc_key(0, 0)), Some(&icc_bytes));
}

#[test]
fn dicom_manifest_rejects_conflicting_icc_profiles_across_volume_instances() {
    let dir = tempfile::tempdir().unwrap();
    let first_path = dir.path().join("first.dcm");
    let mut first_options = TestDicomOptions::native(test_rgb_pixel_data());
    first_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.1";
    first_options.optical_path_icc_profiles = vec![test_optical_path_icc(vec![1, 1, 2, 3])];
    write_test_dicom(&first_path, first_options);
    let second_path = dir.path().join("second.dcm");
    let mut second_options = TestDicomOptions::native(test_rgb_pixel_data());
    second_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.2";
    second_options.optical_path_icc_profiles = vec![test_optical_path_icc(vec![5, 8, 13, 21])];
    write_test_dicom(&second_path, second_options);

    let err = match DicomSlide::parse(dir.path()) {
        Ok(_) => panic!("conflicting instance ICC profiles should fail"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(
        message.contains("different ICC profiles for the same DICOM optical path key"),
        "got: {message}"
    );
    assert!(
        message.contains("1.2.826.0.1.3680043.10.777.2"),
        "got: {message}"
    );
}

#[test]
fn dicom_manifest_dedupes_matching_multi_optical_path_icc_profiles_across_volume_instances() {
    let dir = tempfile::tempdir().unwrap();
    write_two_optical_path_icc_instances(
        dir.path(),
        vec![
            test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
            test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
        ],
        vec![
            test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
            test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
        ],
    );

    let slide = Slide::open(dir.path()).expect("open DICOM directory");
    assert_two_optical_path_source_icc_profiles(slide.dataset());
}

#[test]
fn dicom_manifest_matches_optical_path_icc_profiles_by_identifier_across_reordered_instances() {
    let dir = tempfile::tempdir().unwrap();
    write_two_optical_path_icc_instances(
        dir.path(),
        vec![
            test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
            test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
        ],
        vec![
            test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
            test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
        ],
    );

    let slide = Slide::open(dir.path()).expect("open DICOM directory");
    let dataset = slide.dataset();
    assert_two_optical_path_source_icc_profiles(dataset);
    match &dataset.source_icc_profiles[0].provenance {
        IccProfileProvenance::DicomOpticalPath {
            optical_path_identifier,
            ..
        } => assert_eq!(optical_path_identifier.as_deref(), Some("path-a")),
        other => panic!("unexpected ICC provenance: {other:?}"),
    }
    match &dataset.source_icc_profiles[1].provenance {
        IccProfileProvenance::DicomOpticalPath {
            optical_path_identifier,
            ..
        } => assert_eq!(optical_path_identifier.as_deref(), Some("path-b")),
        other => panic!("unexpected ICC provenance: {other:?}"),
    }
}

#[test]
fn dicom_manifest_drops_unqualified_icc_when_qualified_profiles_exist_for_series() {
    let dir = tempfile::tempdir().unwrap();
    write_two_optical_path_icc_instances(
        dir.path(),
        vec![
            test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
            test_optical_path_icc_with_identifier("path-b", vec![1, 2, 3, 4]),
        ],
        vec![
            test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
            test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
        ],
    );

    let slide = Slide::open(dir.path()).expect("open DICOM directory");
    assert_two_optical_path_source_icc_profiles(slide.dataset());
}

pub(super) fn write_series_level(
    path: &Path,
    sop_instance_uid: &'static str,
    total_rows: u32,
    total_columns: u32,
) {
    let mut options = TestDicomOptions::native(vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);
    options.sop_instance_uid = sop_instance_uid;
    options.rows = 2;
    options.columns = 2;
    options.total_pixel_matrix_rows = total_rows;
    options.total_pixel_matrix_columns = total_columns;
    options.number_of_frames = total_rows.div_ceil(2) * total_columns.div_ceil(2);
    write_test_dicom(path, options);
}

pub(super) fn series_level_dimensions(slide: &Slide) -> Vec<(u64, u64)> {
    slide.dataset().scenes[0].series[0]
        .levels
        .iter()
        .map(|level| level.dimensions)
        .collect()
}

#[test]
fn opens_complete_sibling_series_from_any_member_file() {
    let dir = tempfile::tempdir().unwrap();
    let level0 = dir.path().join("level0.dcm");
    let level1 = dir.path().join("level1.dcm");
    let thumbnail = dir.path().join("thumbnail.dcm");

    write_series_level(&level0, "1.2.826.0.1.3680043.10.777.1", 16, 16);
    write_series_level(&level1, "1.2.826.0.1.3680043.10.777.2", 4, 4);
    let mut thumbnail_options =
        TestDicomOptions::native(vec![32, 32, 32, 64, 64, 64, 96, 96, 96, 128, 128, 128]);
    thumbnail_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.3";
    thumbnail_options.image_type = "DERIVED\\PRIMARY\\THUMBNAIL\\RESAMPLED";
    write_test_dicom(&thumbnail, thumbnail_options);

    let from_base = Slide::open(&level0).expect("open base member");
    let from_coarse = Slide::open(&level1).expect("open coarse member");
    let from_associated = Slide::open(&thumbnail).expect("open associated member");

    assert_eq!(series_level_dimensions(&from_base), vec![(16, 16), (4, 4)]);
    assert_eq!(
        series_level_dimensions(&from_coarse),
        vec![(16, 16), (4, 4)]
    );
    assert_eq!(
        series_level_dimensions(&from_associated),
        vec![(16, 16), (4, 4)]
    );
    assert!(from_associated
        .dataset()
        .associated_images
        .contains_key("thumbnail"));
}

#[test]
fn opens_directory_containing_one_dicom_series() {
    let dir = tempfile::tempdir().unwrap();
    let level0 = dir.path().join("level0.dcm");
    let level1 = dir.path().join("level1.dcm");
    write_series_level(&level0, "1.2.826.0.1.3680043.10.777.1", 16, 16);
    write_series_level(&level1, "1.2.826.0.1.3680043.10.777.2", 4, 4);

    let from_file = Slide::open(&level0).expect("open DICOM member");
    let from_directory = Slide::open(dir.path()).expect("open DICOM series directory");

    assert_eq!(
        series_level_dimensions(&from_directory),
        series_level_dimensions(&from_file)
    );
}

#[test]
fn opens_public_dicom_folder_and_member_with_matching_levels_when_available() {
    let bench_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let candidates = [
        bench_root
            .join("SlideViewer")
            .join("downloads/openslide-testdata-extracted/full/DICOM/CMU-1-JP2K-33005"),
        bench_root.join("downloads/openslide-testdata-extracted/full/DICOM/CMU-1-JP2K-33005"),
    ];
    let Some(folder) = candidates.iter().find(|path| path.is_dir()) else {
        eprintln!("skipping public DICOM folder test; CMU-1-JP2K-33005 not found");
        return;
    };
    let member = std::fs::read_dir(folder)
        .expect("read DICOM folder")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dcm"))
        })
        .expect("public DICOM folder contains a .dcm member");

    let from_folder = Slide::open(folder).expect("open public DICOM folder");
    let from_member = Slide::open(&member).expect("open public DICOM member");

    assert!(
        series_level_dimensions(&from_folder).len() > 1,
        "public DICOM folder should expose physical pyramid levels"
    );
    assert_eq!(
        series_level_dimensions(&from_folder),
        series_level_dimensions(&from_member)
    );
}

#[test]
fn opens_3dhistech_split_sparse_level_when_corpus_is_available() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let path =
        workspace_root.join("downloads/openslide-testdata-extracted/full/DICOM/3DHISTECH-2/2");
    if !path.exists() {
        return;
    }

    let slide = Slide::open(&path).expect("open split-level DICOM slide");
    let dataset = slide.dataset();
    assert_eq!(dataset.scenes.len(), 1);
    assert!(!dataset.scenes[0].series[0].levels.is_empty());
    let tile = slide
        .read_tile(
            &TileRequest {
                scene: 0usize.into(),
                series: 0usize.into(),
                level: 0u32.into(),
                plane: PlaneSelection::default().into(),
                col: 0,
                row: 0,
            },
            TileOutputPreference::cpu(),
        )
        .expect("read first split-level tile");
    assert!(matches!(tile, TilePixels::Cpu(_)));
}
