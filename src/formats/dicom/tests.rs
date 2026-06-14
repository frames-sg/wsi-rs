use super::*;
use crate::core::registry::Slide;
use dicom_core::value::fragments::Fragments;
use dicom_core::value::DataSetSequence;
use dicom_core::value::{PixelFragmentSequence, Value};
use dicom_core::{DataElement, PrimitiveValue, VR};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

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
    assert_eq!(
        parse_level0_properties_from_metadata(&meta),
        parse_level0_properties(&path).expect("level0 property parse")
    );
}

enum TestPixelData {
    Native(Vec<u8>),
    Encapsulated(Vec<u8>),
    EncapsulatedFrames(Vec<Vec<u8>>),
}

struct TestOpticalPathIccProfile {
    optical_path_identifier: Option<&'static str>,
    bytes: Vec<u8>,
}

fn icc_key(scene: usize, series: usize) -> IccProfileKey {
    IccProfileKey::new(SceneId::new(scene), SeriesId::new(series))
}

struct TestDicomOptions {
    sop_instance_uid: &'static str,
    series_instance_uid: &'static str,
    image_type: &'static str,
    transfer_syntax: &'static str,
    samples_per_pixel: u16,
    photometric_interpretation: &'static str,
    planar_configuration: Option<u16>,
    rows: u16,
    columns: u16,
    total_pixel_matrix_rows: u32,
    total_pixel_matrix_columns: u32,
    number_of_frames: u32,
    pixel_spacing: Option<&'static str>,
    shared_pixel_spacing: Option<&'static str>,
    optical_path_icc_profiles: Vec<TestOpticalPathIccProfile>,
    pixel_data: TestPixelData,
}

impl TestDicomOptions {
    fn native(pixel_data: Vec<u8>) -> Self {
        Self {
            sop_instance_uid: "1.2.826.0.1.3680043.10.777.1",
            series_instance_uid: "1.2.826.0.1.3680043.10.777",
            image_type: "ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            transfer_syntax: uids::EXPLICIT_VR_LITTLE_ENDIAN,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            rows: 2,
            columns: 2,
            total_pixel_matrix_rows: 2,
            total_pixel_matrix_columns: 2,
            number_of_frames: 1,
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            optical_path_icc_profiles: Vec::new(),
            pixel_data: TestPixelData::Native(pixel_data),
        }
    }
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

fn test_rgb_pixel_data() -> Vec<u8> {
    vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]
}

fn write_test_dicom(path: &Path, options: TestDicomOptions) {
    let mut object = InMemDicomObject::new_empty();
    object.put(DataElement::new(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE,
    ));
    object.put(DataElement::new(
        tags::SOP_INSTANCE_UID,
        VR::UI,
        options.sop_instance_uid,
    ));
    object.put(DataElement::new(
        tags::SERIES_INSTANCE_UID,
        VR::UI,
        options.series_instance_uid,
    ));
    object.put(DataElement::new(
        tags::IMAGE_TYPE,
        VR::CS,
        options.image_type,
    ));
    object.put(DataElement::new(
        tags::ROWS,
        VR::US,
        PrimitiveValue::from(options.rows),
    ));
    object.put(DataElement::new(
        tags::COLUMNS,
        VR::US,
        PrimitiveValue::from(options.columns),
    ));
    object.put(DataElement::new(
        tags::TOTAL_PIXEL_MATRIX_ROWS,
        VR::UL,
        PrimitiveValue::from(options.total_pixel_matrix_rows),
    ));
    object.put(DataElement::new(
        tags::TOTAL_PIXEL_MATRIX_COLUMNS,
        VR::UL,
        PrimitiveValue::from(options.total_pixel_matrix_columns),
    ));
    object.put(DataElement::new(
        tags::NUMBER_OF_FRAMES,
        VR::IS,
        PrimitiveValue::from(options.number_of_frames),
    ));
    object.put(DataElement::new(
        tags::SAMPLES_PER_PIXEL,
        VR::US,
        PrimitiveValue::from(options.samples_per_pixel),
    ));
    object.put(DataElement::new(
        tags::PHOTOMETRIC_INTERPRETATION,
        VR::CS,
        options.photometric_interpretation,
    ));
    if let Some(planar_configuration) = options.planar_configuration {
        object.put(DataElement::new(
            tags::PLANAR_CONFIGURATION,
            VR::US,
            PrimitiveValue::from(planar_configuration),
        ));
    }
    object.put(DataElement::new(
        tags::BITS_ALLOCATED,
        VR::US,
        PrimitiveValue::from(8u16),
    ));
    object.put(DataElement::new(
        tags::BITS_STORED,
        VR::US,
        PrimitiveValue::from(8u16),
    ));
    object.put(DataElement::new(
        tags::HIGH_BIT,
        VR::US,
        PrimitiveValue::from(7u16),
    ));
    object.put(DataElement::new(
        tags::PIXEL_REPRESENTATION,
        VR::US,
        PrimitiveValue::from(0u16),
    ));
    if let Some(pixel_spacing) = options.pixel_spacing {
        object.put(DataElement::new(tags::PIXEL_SPACING, VR::DS, pixel_spacing));
    }
    if let Some(pixel_spacing) = options.shared_pixel_spacing {
        let mut pixel_measures = InMemDicomObject::new_empty();
        pixel_measures.put(DataElement::new(tags::PIXEL_SPACING, VR::DS, pixel_spacing));
        let mut shared = InMemDicomObject::new_empty();
        shared.put(DataElement::<InMemDicomObject>::new(
            tags::PIXEL_MEASURES_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(vec![pixel_measures]),
        ));
        object.put(DataElement::<InMemDicomObject>::new(
            tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(vec![shared]),
        ));
    }
    if !options.optical_path_icc_profiles.is_empty() {
        let optical_paths = options
            .optical_path_icc_profiles
            .into_iter()
            .map(|profile| {
                let mut optical_path = InMemDicomObject::new_empty();
                if let Some(identifier) = profile.optical_path_identifier {
                    optical_path.put(DataElement::new(
                        tags::OPTICAL_PATH_IDENTIFIER,
                        VR::SH,
                        identifier,
                    ));
                }
                optical_path.put(DataElement::new(
                    tags::ICC_PROFILE,
                    VR::OB,
                    PrimitiveValue::from(profile.bytes),
                ));
                optical_path
            })
            .collect::<Vec<_>>();
        object.put(DataElement::<InMemDicomObject>::new(
            tags::OPTICAL_PATH_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(optical_paths),
        ));
    }
    match options.pixel_data {
        TestPixelData::Native(pixel_data) => {
            object.put(DataElement::new(
                tags::PIXEL_DATA,
                VR::OB,
                PrimitiveValue::from(pixel_data),
            ));
        }
        TestPixelData::Encapsulated(frame) => {
            let pixel_sequence = PixelFragmentSequence::from(vec![Fragments::new(frame, 0)]);
            object.put(DataElement::<InMemDicomObject>::new(
                tags::PIXEL_DATA,
                VR::OB,
                Value::from(pixel_sequence),
            ));
        }
        TestPixelData::EncapsulatedFrames(frames) => {
            let fragments = frames
                .into_iter()
                .map(|frame| Fragments::new(frame, 0))
                .collect::<Vec<_>>();
            let pixel_sequence = PixelFragmentSequence::from(fragments);
            object.put(DataElement::<InMemDicomObject>::new(
                tags::PIXEL_DATA,
                VR::OB,
                Value::from(pixel_sequence),
            ));
        }
    }
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .media_storage_sop_class_uid(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
                .media_storage_sop_instance_uid(options.sop_instance_uid)
                .transfer_syntax(options.transfer_syntax),
        )
        .unwrap()
        .write_to_file(path)
        .unwrap();
}

fn read_first_tile(path: &Path) -> CpuTile {
    let slide = Slide::open(path).expect("open DICOM slide");
    match slide
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
        .expect("read first tile")
    {
        TilePixels::Cpu(tile) => tile,
        TilePixels::Device(_) => panic!("DICOM tests request CPU output"),
    }
}

fn read_first_raw_compressed_tile(path: &Path) -> RawCompressedTile {
    Slide::open(path)
        .expect("open DICOM slide")
        .read_raw_compressed_tile(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .expect("read first raw compressed tile")
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
    let first_path = dir.path().join("first.dcm");
    let mut first_options = TestDicomOptions::native(test_rgb_pixel_data());
    first_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.1";
    first_options.optical_path_icc_profiles = vec![
        test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
        test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
    ];
    write_test_dicom(&first_path, first_options);
    let second_path = dir.path().join("second.dcm");
    let mut second_options = TestDicomOptions::native(test_rgb_pixel_data());
    second_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.2";
    second_options.optical_path_icc_profiles = vec![
        test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
        test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
    ];
    write_test_dicom(&second_path, second_options);

    let slide = Slide::open(dir.path()).expect("open DICOM directory");
    let dataset = slide.dataset();
    assert!(!dataset.icc_profiles.contains_key(&icc_key(0, 0)));
    assert_eq!(dataset.source_icc_profiles.len(), 2);
    assert_eq!(dataset.source_icc_profiles[0].key.optical_path, Some(0));
    assert_eq!(dataset.source_icc_profiles[0].bytes, vec![1, 2, 3, 4]);
    assert_eq!(dataset.source_icc_profiles[1].key.optical_path, Some(1));
    assert_eq!(dataset.source_icc_profiles[1].bytes, vec![5, 8, 13, 21]);
}

#[test]
fn dicom_manifest_matches_optical_path_icc_profiles_by_identifier_across_reordered_instances() {
    let dir = tempfile::tempdir().unwrap();
    let first_path = dir.path().join("first.dcm");
    let mut first_options = TestDicomOptions::native(test_rgb_pixel_data());
    first_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.1";
    first_options.optical_path_icc_profiles = vec![
        test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
        test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
    ];
    write_test_dicom(&first_path, first_options);
    let second_path = dir.path().join("second.dcm");
    let mut second_options = TestDicomOptions::native(test_rgb_pixel_data());
    second_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.2";
    second_options.optical_path_icc_profiles = vec![
        test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
        test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
    ];
    write_test_dicom(&second_path, second_options);

    let slide = Slide::open(dir.path()).expect("open DICOM directory");
    let dataset = slide.dataset();
    assert!(!dataset.icc_profiles.contains_key(&icc_key(0, 0)));
    assert_eq!(dataset.source_icc_profiles.len(), 2);
    assert_eq!(dataset.source_icc_profiles[0].key.optical_path, Some(0));
    assert_eq!(dataset.source_icc_profiles[0].bytes, vec![1, 2, 3, 4]);
    assert_eq!(dataset.source_icc_profiles[1].key.optical_path, Some(1));
    assert_eq!(dataset.source_icc_profiles[1].bytes, vec![5, 8, 13, 21]);
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
    let first_path = dir.path().join("first.dcm");
    let mut first_options = TestDicomOptions::native(test_rgb_pixel_data());
    first_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.1";
    first_options.optical_path_icc_profiles = vec![
        test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
        test_optical_path_icc_with_identifier("path-b", vec![1, 2, 3, 4]),
    ];
    write_test_dicom(&first_path, first_options);
    let second_path = dir.path().join("second.dcm");
    let mut second_options = TestDicomOptions::native(test_rgb_pixel_data());
    second_options.sop_instance_uid = "1.2.826.0.1.3680043.10.777.2";
    second_options.optical_path_icc_profiles = vec![
        test_optical_path_icc_with_identifier("path-a", vec![1, 2, 3, 4]),
        test_optical_path_icc_with_identifier("path-b", vec![5, 8, 13, 21]),
    ];
    write_test_dicom(&second_path, second_options);

    let slide = Slide::open(dir.path()).expect("open DICOM directory");
    let dataset = slide.dataset();
    assert!(!dataset.icc_profiles.contains_key(&icc_key(0, 0)));
    assert_eq!(dataset.source_icc_profiles.len(), 2);
    assert_eq!(dataset.source_icc_profiles[0].key.optical_path, Some(0));
    assert_eq!(dataset.source_icc_profiles[0].bytes, vec![1, 2, 3, 4]);
    assert_eq!(dataset.source_icc_profiles[1].key.optical_path, Some(1));
    assert_eq!(dataset.source_icc_profiles[1].bytes, vec![5, 8, 13, 21]);
}

fn test_dicom_image(sop_instance_uid: &str, grid: DicomGrid) -> Arc<DicomImage> {
    test_dicom_image_with_transfer_syntax(sop_instance_uid, grid, uids::EXPLICIT_VR_LITTLE_ENDIAN)
}

fn test_dicom_image_with_transfer_syntax(
    sop_instance_uid: &str,
    grid: DicomGrid,
    transfer_syntax_uid: &str,
) -> Arc<DicomImage> {
    Arc::new(DicomImage {
        path: PathBuf::from(format!("{sop_instance_uid}.dcm")),
        sop_instance_uid: sop_instance_uid.into(),
        transfer_syntax_uid: transfer_syntax_uid.into(),
        photometric_interpretation: "RGB".into(),
        samples_per_pixel: 3,
        planar_configuration: Some(0),
        width: 4096,
        height: 4096,
        tile_width: 512,
        tile_height: 512,
        tiles_across: 8,
        tiles_down: 8,
        number_of_frames: 1,
        grid,
        pixel_spacing: None,
        objective_lens_power: None,
        encapsulated_frames: Mutex::new(None),
        encapsulated_frame_cache: Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(1).unwrap(),
        )),
        decoded_frame_cache: Mutex::new(LruCache::new(std::num::NonZeroUsize::new(1).unwrap())),
    })
}

fn empty_dataset() -> Dataset {
    Dataset {
        id: DatasetId::new(1),
        scenes: Vec::new(),
        associated_images: HashMap::new(),
        properties: Properties::new(),
        icc_profiles: HashMap::new(),
        source_icc_profiles: Vec::new(),
    }
}

fn tile_request(col: i64, row: i64) -> TileRequest {
    TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col,
        row,
    }
}

fn encode_test_jpeg_rgb(width: u16, height: u16, seed: u8) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height {
        for x in 0..width {
            let base = seed
                .wrapping_add(x as u8)
                .wrapping_add((y as u8).wrapping_mul(3));
            rgb.extend_from_slice(&[base, base.wrapping_add(17), base.wrapping_add(31)]);
        }
    }
    let mut encoded = Vec::new();
    jpeg_encoder::Encoder::new(&mut encoded, 90)
        .encode(&rgb, width, height, jpeg_encoder::ColorType::Rgb)
        .expect("encode baseline JPEG test frame");
    encoded
}

#[cfg(feature = "metal")]
fn test_metal_sessions() -> Option<crate::output::metal::MetalBackendSessions> {
    let device = metal::Device::system_default()?;
    Some(crate::output::metal::MetalBackendSessions::new(device))
}

fn rgb_bytes(tile: &CpuTile) -> Vec<u8> {
    assert_eq!(tile.width, 2);
    assert_eq!(tile.height, 2);
    assert_eq!(tile.channels, 3);
    assert_eq!(tile.color_space, ColorSpace::Rgb);
    assert_eq!(tile.layout, CpuTileLayout::Interleaved);
    tile.data.as_u8().expect("u8 RGB tile").to_vec()
}

#[test]
fn crop_sample_buffer_rgb_borrows_source_and_preserves_contiguous_rows() {
    let source = CpuTile::from_u8_interleaved(
        3,
        2,
        3,
        ColorSpace::Rgb,
        vec![
            1, 2, 3, 4, 5, 6, 7, 8, 9, //
            10, 11, 12, 13, 14, 15, 16, 17, 18,
        ],
    )
    .expect("source tile");

    let cropped = crop_sample_buffer_rgb(&source, 2, 2).expect("crop borrowed source");

    assert_eq!(source.width, 3, "source tile remains available after crop");
    assert_eq!(cropped.width, 2);
    assert_eq!(cropped.height, 2);
    assert_eq!(
        cropped.data.as_u8().expect("cropped RGB"),
        &[1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15]
    );
}

fn reader_and_first_image(path: &Path) -> (DicomReader, Arc<DicomImage>) {
    let slide = Arc::new(DicomSlide::parse(path).expect("parse generated DICOM slide"));
    let image = slide.levels[0].parts[0].clone();
    (DicomReader { slide }, image)
}

fn assert_cached_edge_frame_crop(path: &Path, expected_width: u32, expected_height: u32) {
    let (reader, image) = reader_and_first_image(path);
    let req = tile_request(1, 0);

    assert!(
        image.cached_decoded_frame(1).is_none(),
        "test must start without a cached edge frame"
    );
    let first = reader.read_tile_cpu(&req).expect("read edge tile");
    assert!(
        image.cached_decoded_frame(1).is_some(),
        "first read should cache the full decoded frame"
    );
    let second = reader.read_tile_cpu(&req).expect("read cached edge tile");

    assert_eq!(
        (first.width, first.height),
        (expected_width, expected_height)
    );
    assert_eq!(
        (second.width, second.height),
        (expected_width, expected_height)
    );
    assert_eq!(
        first.data.as_u8().expect("first edge tile"),
        second.data.as_u8().expect("second edge tile"),
        "cached full frame crop must match the first edge-frame crop"
    );
}

#[test]
fn cached_jpeg_edge_frame_preserves_cropped_dimensions_and_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-edge-cache.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 24;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        encode_test_jpeg_rgb(16, 16, 3),
        encode_test_jpeg_rgb(16, 16, 41),
    ]);
    write_test_dicom(&path, options);

    assert_cached_edge_frame_crop(&path, 8, 16);
}

#[test]
fn cached_jp2k_edge_frame_preserves_cropped_dimensions_and_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-edge-cache.dcm");
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 24;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![codestream.clone(), codestream]);
    write_test_dicom(&path, options);

    assert_cached_edge_frame_crop(&path, 8, 12);
}

#[test]
fn cached_rle_edge_frame_preserves_cropped_dimensions_and_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rle-edge-cache.dcm");
    let pixels = 16usize;
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = RLE_TRANSFER_SYNTAX;
    options.rows = 4;
    options.columns = 4;
    options.total_pixel_matrix_rows = 4;
    options.total_pixel_matrix_columns = 6;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        rle_rgb_frame(&vec![10; pixels], &vec![20; pixels], &vec![30; pixels]),
        rle_rgb_frame(&vec![40; pixels], &vec![50; pixels], &vec![60; pixels]),
    ]);
    write_test_dicom(&path, options);

    assert_cached_edge_frame_crop(&path, 2, 4);
}

fn write_series_level(
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

fn series_level_dimensions(slide: &Slide) -> Vec<(u64, u64)> {
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

#[test]
fn compressed_dicom_recommends_cache_for_common_read_region_working_set() {
    let levels = build_levels(
        Path::new("cache-hint.dcm"),
        vec![test_dicom_image_with_transfer_syntax(
            "1.2.3.1",
            DicomGrid::Full,
            uids::JPEG2000_LOSSLESS,
        )],
    )
    .expect("level should build");
    let reader = DicomReader {
        slide: Arc::new(DicomSlide {
            dataset: empty_dataset(),
            levels,
            associated: HashMap::new(),
        }),
    };

    assert_eq!(
        reader.recommended_shared_cache_bytes(),
        Some(12 * 1024 * 1024)
    );
}

#[test]
fn native_dicom_keeps_default_shared_cache_hint() {
    let levels = build_levels(
        Path::new("native-cache-hint.dcm"),
        vec![test_dicom_image_with_transfer_syntax(
            "1.2.3.1",
            DicomGrid::Full,
            uids::EXPLICIT_VR_LITTLE_ENDIAN,
        )],
    )
    .expect("level should build");
    let reader = DicomReader {
        slide: Arc::new(DicomSlide {
            dataset: empty_dataset(),
            levels,
            associated: HashMap::new(),
        }),
    };

    assert_eq!(reader.recommended_shared_cache_bytes(), None);
}

#[test]
#[cfg(feature = "metal")]
fn require_device_rejects_sparse_missing_dicom_tile_cpu_black_fallback() {
    let Some(sessions) = test_metal_sessions() else {
        return;
    };
    let mut present_tiles = HashMap::new();
    present_tiles.insert((0, 0), 0);
    let levels = build_levels(
        Path::new("sparse-device.dcm"),
        vec![test_dicom_image_with_transfer_syntax(
            "1.2.3.1",
            DicomGrid::Sparse(present_tiles),
            uids::JPEG2000_LOSSLESS,
        )],
    )
    .expect("sparse level should build");
    let reader = DicomReader {
        slide: Arc::new(DicomSlide {
            dataset: empty_dataset(),
            levels,
            associated: HashMap::new(),
        }),
    };

    let err = reader
        .read_tiles(
            &[tile_request(1, 0)],
            TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions),
        )
        .expect_err("RequireDevice must not return CPU black sparse tile");

    assert!(matches!(err, WsiError::Unsupported { .. }));
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

#[test]
fn read_tiles_cpu_decodes_jpeg_frames_in_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-cpu-batch.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 32;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        encode_test_jpeg_rgb(16, 16, 3),
        encode_test_jpeg_rgb(16, 16, 41),
    ]);
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open generated DICOM JPEG slide");
    let tiles = slide
        .read_tiles(
            &[tile_request(1, 0), tile_request(0, 0)],
            TileOutputPreference::cpu(),
        )
        .expect("read JPEG CPU tile batch");

    assert_eq!(tiles.len(), 2);
    let TilePixels::Cpu(first) = &tiles[0] else {
        panic!("CPU output expected");
    };
    let TilePixels::Cpu(second) = &tiles[1] else {
        panic!("CPU output expected");
    };
    assert_ne!(
        first.data.as_u8().expect("first JPEG tile").get(0..3),
        second.data.as_u8().expect("second JPEG tile").get(0..3),
        "request order should be preserved across distinct decoded frames"
    );
}

#[test]
fn read_tiles_cpu_decodes_jp2k_frames_in_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-cpu-batch.dcm");
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 32;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![codestream.clone(), codestream]);
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open generated DICOM JP2K slide");
    let tiles = slide
        .read_tiles(
            &[tile_request(1, 0), tile_request(0, 0)],
            TileOutputPreference::cpu(),
        )
        .expect("read JP2K CPU tile batch");

    assert_eq!(tiles.len(), 2);
    assert!(tiles.iter().all(|tile| matches!(tile, TilePixels::Cpu(_))));
}

#[test]
fn read_tiles_cpu_skips_decoded_cache_when_batch_exceeds_cache_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-cache-churn.dcm");
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 48;
    options.number_of_frames = 3;
    options.pixel_data =
        TestPixelData::EncapsulatedFrames(vec![codestream.clone(), codestream.clone(), codestream]);
    write_test_dicom(&path, options);

    let (reader, image) = reader_and_first_image(&path);
    let tiles = reader
        .read_tiles(
            &[tile_request(0, 0), tile_request(1, 0), tile_request(2, 0)],
            TileOutputPreference::cpu(),
        )
        .expect("read JP2K CPU tile batch");

    assert_eq!(tiles.len(), 3);
    assert!(tiles.iter().all(|tile| matches!(tile, TilePixels::Cpu(_))));
    assert!(
        (0..3).all(|frame_index| image.cached_decoded_frame(frame_index).is_none()),
        "batch larger than the decoded cache should not clone decoded JP2K frames into the LRU"
    );
}

#[test]
fn extract_encapsulated_frames_batch_preserves_requested_frames() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("batch-frames.dcm");
    let frames = vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8], vec![9, 10, 11, 12]];
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 2;
    options.columns = 2;
    options.total_pixel_matrix_rows = 2;
    options.total_pixel_matrix_columns = 6;
    options.number_of_frames = frames.len() as u32;
    options.pixel_data = TestPixelData::EncapsulatedFrames(frames.clone());
    write_test_dicom(&path, options);

    let (_reader, image) = reader_and_first_image(&path);
    let extracted = image
        .extract_encapsulated_frames(&[2, 0], 0, 0, 0, true)
        .expect("batch extract frames");

    assert_eq!(extracted.get(&2).unwrap().as_slice(), frames[2].as_slice());
    assert_eq!(extracted.get(&0).unwrap().as_slice(), frames[0].as_slice());
}

fn literal_rle_segment(bytes: &[u8]) -> Vec<u8> {
    assert!((1..=128).contains(&bytes.len()));
    let mut encoded = Vec::with_capacity(bytes.len() + 1);
    encoded.push((bytes.len() - 1) as u8);
    encoded.extend_from_slice(bytes);
    encoded
}

fn rle_rgb_frame(r: &[u8], g: &[u8], b: &[u8]) -> Vec<u8> {
    let segments = [
        literal_rle_segment(r),
        literal_rle_segment(g),
        literal_rle_segment(b),
    ];
    let mut frame = vec![0; 64];
    frame[0..4].copy_from_slice(&3u32.to_le_bytes());
    let mut offset = 64u32;
    for (idx, segment) in segments.iter().enumerate() {
        let start = 4 + idx * 4;
        frame[start..start + 4].copy_from_slice(&offset.to_le_bytes());
        offset += segment.len() as u32;
    }
    for segment in segments {
        frame.extend_from_slice(&segment);
    }
    frame
}

fn push_explicit_vr_long_element(bytes: &mut Vec<u8>, tag: [u8; 4], vr: &[u8; 2], value: &[u8]) {
    bytes.extend_from_slice(&tag);
    bytes.extend_from_slice(vr);
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
    bytes.extend_from_slice(value);
}

fn push_pixel_fragment(bytes: &mut Vec<u8>, payload: &[u8]) -> u64 {
    let item_offset = bytes.len() as u64;
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    item_offset
}

#[test]
fn raw_encapsulated_scan_handles_extended_offset_table_layout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-eot-htj2k.dcm");
    let first = [0xFF, 0x4F, 0x01, 0x02];
    let second = [0xFF, 0x4F, 0x03, 0x04, 0x05, 0x06];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut eot = Vec::new();
    eot.extend_from_slice(&0u64.to_le_bytes());
    eot.extend_from_slice(&(first.len() as u64 + 8).to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0xE0, 0x7F, 0x01, 0x00], b"OV", &eot);

    let mut eot_lengths = Vec::new();
    eot_lengths.extend_from_slice(&(first.len() as u64).to_le_bytes());
    eot_lengths.extend_from_slice(&(second.len() as u64).to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0xE0, 0x7F, 0x02, 0x00], b"OV", &eot_lengths);

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let first_item_offset = push_pixel_fragment(&mut bytes, &first);
    let second_item_offset = push_pixel_fragment(&mut bytes, &second);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let frames = scan_encapsulated_frames_raw_little_endian(&path, 2)
        .expect("raw scan succeeds")
        .expect("Pixel Data is found");
    assert_eq!(frames.frame_ranges, vec![0..1, 1..2]);
    assert_eq!(frames.fragments.len(), 2);
    assert_eq!(frames.fragments[0].item_offset, first_item_offset);
    assert_eq!(frames.fragments[0].len, first.len() as u32);
    assert_eq!(frames.fragments[1].item_offset, second_item_offset);
    assert_eq!(frames.fragments[1].len, second.len() as u32);
}

#[test]
fn large_basic_offset_table_frame_index_builds_quickly() {
    let frame_count = 25_000usize;
    let mut fragments = Vec::with_capacity(frame_count);
    let mut offset_table = Vec::with_capacity(frame_count);
    let mut item_offset = 1024u64;
    for _ in 0..frame_count {
        offset_table.push((item_offset - 1024) as u32);
        fragments.push(DicomFragmentRef {
            payload_offset: item_offset + 8,
            item_offset,
            len: 64,
        });
        item_offset += 72;
    }

    let started = std::time::Instant::now();
    let frames = build_encapsulated_frame_index(
        Path::new("large-basic-offset-table.dcm"),
        fragments,
        offset_table,
        frame_count as u32,
    )
    .expect("large basic offset table should build");

    assert_eq!(frames.frame_ranges.len(), frame_count);
    assert_eq!(frames.frame_ranges[0], 0..1);
    assert_eq!(
        frames.frame_ranges[frame_count - 1],
        frame_count - 1..frame_count
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(250),
        "large DICOM basic offset table frame index should build in linear time"
    );
}

#[test]
#[cfg(feature = "metal")]
fn local_htj2k_dicom_full_tile_can_require_device_output() {
    let Some(path) = local_htj2k_dicom_device_fixture() else {
        return;
    };
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping local HTJ2K DICOM device test; no Metal device");
        return;
    };

    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
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
            TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions),
        )
        .expect("read full HTJ2K tile with required device output");

    assert!(matches!(tile, TilePixels::Device(_)));
}

#[test]
#[cfg(feature = "metal")]
fn local_htj2k_dicom_prefer_device_batch_keeps_full_tiles_on_device() {
    let Some(path) = local_htj2k_dicom_device_fixture() else {
        return;
    };
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping local HTJ2K DICOM device test; no Metal device");
        return;
    };

    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let tiles = slide
        .read_tiles(
            &[
                TileRequest {
                    scene: 0usize.into(),
                    series: 0usize.into(),
                    level: 0u32.into(),
                    plane: PlaneSelection::default().into(),
                    col: 0,
                    row: 0,
                },
                TileRequest {
                    scene: 0usize.into(),
                    series: 0usize.into(),
                    level: 0u32.into(),
                    plane: PlaneSelection::default().into(),
                    col: 1,
                    row: 0,
                },
            ],
            TileOutputPreference::prefer_device_auto_with_metal_and_compressed_decode(sessions)
                .without_adaptive_decode_route(),
        )
        .expect("read full HTJ2K tile batch with residency-preferred device output");

    assert!(
        tiles
            .iter()
            .any(|tile| matches!(tile, TilePixels::Device(_))),
        "prefer-device HTJ2K batch should return device tiles when full tiles are decodable"
    );
}

#[test]
#[cfg(all(feature = "metal", target_os = "macos"))]
fn dicom_jpeg_require_device_batch_uses_jpeg_device_route() {
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping DICOM JPEG device batch test; no Metal device");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-batch.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 32;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        encode_test_jpeg_rgb(16, 16, 3),
        encode_test_jpeg_rgb(16, 16, 41),
    ]);
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open generated DICOM JPEG slide");
    let tiles = slide
        .read_tiles(
            &[tile_request(0, 0), tile_request(1, 0)],
            TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions)
                .without_adaptive_decode_route(),
        )
        .expect("DICOM JPEG full-tile batch should support required device output");

    assert_eq!(tiles.len(), 2);
    assert!(
        tiles
            .iter()
            .all(|tile| matches!(tile, TilePixels::Device(_))),
        "DICOM JPEG batch should keep all full tiles on device"
    );
}

#[cfg(feature = "metal")]
fn local_htj2k_dicom_device_fixture() -> Option<PathBuf> {
    let Some(path) = std::env::var_os("STATUMEN_LOCAL_HTJ2K_DICOM").map(PathBuf::from) else {
        eprintln!("skipping local HTJ2K DICOM device test; STATUMEN_LOCAL_HTJ2K_DICOM unset");
        return None;
    };
    if !path.is_file() {
        eprintln!(
            "skipping local HTJ2K DICOM device test; missing {}",
            path.display()
        );
        return None;
    }
    Some(path)
}

#[test]
fn opens_implicit_vr_little_endian_native_rgb() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("implicit.dcm");
    let mut options = TestDicomOptions::native(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    options.transfer_syntax = uids::IMPLICIT_VR_LITTLE_ENDIAN;
    write_test_dicom(&path, options);

    assert_eq!(
        rgb_bytes(&read_first_tile(&path)),
        vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]
    );
}

#[test]
fn opens_explicit_vr_big_endian_native_rgb_8bit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big-endian.dcm");
    let mut options = TestDicomOptions::native(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    options.transfer_syntax = EXPLICIT_VR_BIG_ENDIAN_TRANSFER_SYNTAX;
    write_test_dicom(&path, options);

    assert_eq!(
        rgb_bytes(&read_first_tile(&path)),
        vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]
    );
}

#[test]
fn converts_planar_rgb_native_frames_to_interleaved_rgb() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("planar.dcm");
    let mut options = TestDicomOptions::native(vec![
        255, 0, 0, 255, // R plane
        0, 255, 0, 255, // G plane
        0, 0, 255, 0, // B plane
    ]);
    options.planar_configuration = Some(1);
    write_test_dicom(&path, options);

    assert_eq!(
        rgb_bytes(&read_first_tile(&path)),
        vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]
    );
}

#[test]
fn expands_monochrome_8bit_native_frames_to_rgb() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mono.dcm");
    let mut options = TestDicomOptions::native(vec![0, 64, 128, 255]);
    options.samples_per_pixel = 1;
    options.photometric_interpretation = "MONOCHROME2";
    options.planar_configuration = None;
    write_test_dicom(&path, options);

    assert_eq!(
        rgb_bytes(&read_first_tile(&path)),
        vec![0, 0, 0, 64, 64, 64, 128, 128, 128, 255, 255, 255]
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
fn decodes_rle_lossless_rgb_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rle.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: uids::RLE_LOSSLESS,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(1),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(rle_rgb_frame(
                &[255, 0, 0, 255],
                &[0, 255, 0, 255],
                &[0, 0, 255, 0],
            )),
            ..TestDicomOptions::native(Vec::new())
        },
    );

    assert_eq!(
        rgb_bytes(&read_first_tile(&path)),
        vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]
    );
}

#[test]
fn reads_htj2k_rpcl_raw_compressed_frame_without_dicom_padding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("htj2k-rpcl.dcm");
    let codestream = vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9];
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(codestream.clone()),
            ..TestDicomOptions::native(Vec::new())
        },
    );

    let raw = read_first_raw_compressed_tile(&path);
    assert_eq!(raw.compression(), Compression::Jp2kRgb);
    assert_eq!(raw.width(), 2);
    assert_eq!(raw.height(), 2);
    assert_eq!(raw.bits_allocated(), 8);
    assert_eq!(raw.samples_per_pixel(), 3);
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::Rgb
    );
    assert_eq!(raw.data(), codestream);
}

#[test]
fn reads_htj2k_rpcl_ybr_full_raw_compressed_frame_as_ycbcr() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("htj2k-rpcl-ybr-full.dcm");
    let codestream = vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9];
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "YBR_FULL",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(codestream.clone()),
            ..TestDicomOptions::native(Vec::new())
        },
    );

    let raw = read_first_raw_compressed_tile(&path);
    assert_eq!(raw.compression(), Compression::Jp2kYcbcr);
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::YbrFull422
    );
    assert_eq!(raw.data(), codestream);
}

#[test]
fn reads_general_htj2k_ybr_full_raw_compressed_frame_as_ycbcr() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("htj2k-general-ybr-full.dcm");
    let codestream = vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9];
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: "1.2.840.10008.1.2.4.203",
            samples_per_pixel: 3,
            photometric_interpretation: "YBR_FULL",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(codestream.clone()),
            ..TestDicomOptions::native(Vec::new())
        },
    );

    let raw = read_first_raw_compressed_tile(&path);
    assert_eq!(raw.compression(), Compression::Jp2kYcbcr);
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::YbrFull422
    );
    assert_eq!(raw.data(), codestream);
}

#[test]
fn reads_legacy_htj2k_ybr_full_422_raw_compressed_frame_as_ycbcr() {
    for transfer_syntax in [HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX, HTJ2K_TRANSFER_SYNTAX] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("htj2k-legacy-ybr-full-422.dcm");
        let codestream = vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9];
        write_test_dicom(
            &path,
            TestDicomOptions {
                transfer_syntax,
                samples_per_pixel: 3,
                photometric_interpretation: "YBR_FULL_422",
                planar_configuration: Some(0),
                pixel_spacing: Some("0.00025\\0.00025"),
                shared_pixel_spacing: None,
                pixel_data: TestPixelData::Encapsulated(codestream.clone()),
                ..TestDicomOptions::native(Vec::new())
            },
        );

        let raw = read_first_raw_compressed_tile(&path);
        assert_eq!(raw.compression(), Compression::Jp2kYcbcr);
        assert_eq!(
            raw.photometric_interpretation(),
            EncodedTilePhotometricInterpretation::YbrFull422
        );
        assert_eq!(raw.data(), codestream);
    }
}

#[test]
fn dicom_parse_keeps_encapsulated_frame_index_lazy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("htj2k-rpcl.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );

    let slide = DicomSlide::parse(&path).expect("parse DICOM slide");

    let image = &slide.levels[0].parts[0];
    assert!(
        image
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "encapsulated frame index should stay lazy until first frame read"
    );
}

#[test]
fn tile_codec_kind_classifies_dicom_transfer_syntaxes() {
    assert_eq!(
        dicom_tile_codec_kind(JPEG_TRANSFER_SYNTAX),
        TileCodecKind::Jpeg
    );
    assert_eq!(
        dicom_tile_codec_kind(uids::JPEG2000_LOSSLESS),
        TileCodecKind::Jp2k
    );
    assert_eq!(
        dicom_tile_codec_kind(HTJ2K_LOSSLESS_TRANSFER_SYNTAX),
        TileCodecKind::Htj2k
    );
    assert_eq!(
        dicom_tile_codec_kind(HTJ2K_TRANSFER_SYNTAX),
        TileCodecKind::Htj2k
    );
    assert_eq!(
        dicom_tile_codec_kind(uids::EXPLICIT_VR_LITTLE_ENDIAN),
        TileCodecKind::Other
    );
}

#[test]
#[cfg(feature = "metal")]
fn dicom_jp2k_device_batch_policy_is_selective() {
    let prefer_device = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    let explicit_device = TileOutputPreference::prefer_device_auto_with_compressed_decode()
        .without_adaptive_decode_route();
    let require_device = TileOutputPreference::require_device_auto_with_compressed_decode();

    assert!(dicom_jp2k_device_batch_allowed_for_output(
        HTJ2K_LOSSLESS_TRANSFER_SYNTAX,
        &prefer_device,
        false,
        1,
    ));
    assert!(!dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &prefer_device,
        false,
        4,
    ));
    assert!(dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &prefer_device,
        false,
        8,
    ));
    assert!(dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &explicit_device,
        false,
        1,
    ));
    assert!(dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &require_device,
        false,
        1,
    ));
    assert!(dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &prefer_device,
        true,
        1,
    ));
}

#[test]
fn reads_jpeg2000_ybr_rct_raw_compressed_frame_as_ycbcr() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg2000-ybr-rct.dcm");
    let codestream = vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9];
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: uids::JPEG2000_LOSSLESS,
            samples_per_pixel: 3,
            photometric_interpretation: "YBR_RCT",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(codestream.clone()),
            ..TestDicomOptions::native(Vec::new())
        },
    );

    let raw = read_first_raw_compressed_tile(&path);
    assert_eq!(raw.compression(), Compression::Jp2kYcbcr);
    assert_eq!(raw.width(), 2);
    assert_eq!(raw.height(), 2);
    assert_eq!(raw.bits_allocated(), 8);
    assert_eq!(raw.samples_per_pixel(), 3);
    assert_eq!(
        raw.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::YbrFull422
    );
    assert_eq!(raw.data(), codestream);
}
