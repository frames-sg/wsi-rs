use super::*;

#[test]
fn private_frame_cache_capacity_tracks_cache_config_bytes() {
    let entry_bytes = dicom_frame_cache_entry_bytes(16, 16, 3);
    let mut small_budget = CacheConfig::deterministic()
        .with_shared_tile_bytes(12 * 1024)
        .private_cache_budget(2);
    let mut large_budget = CacheConfig::deterministic()
        .with_shared_tile_bytes(48 * 1024)
        .private_cache_budget(2);
    let small = PrivateCache::<u32, Arc<CpuTile>>::new(small_budget.allocate(entry_bytes));
    let large = PrivateCache::<u32, Arc<CpuTile>>::new(large_budget.allocate(entry_bytes));

    assert_eq!(small.capacity_entries(), 1);
    assert_eq!(large.capacity_entries(), 4);
}

#[test]
fn configured_probe_reuses_the_small_budget_slide_during_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small-budget.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let cache_config = CacheConfig::deterministic().with_shared_tile_bytes(512);
    let backend = DicomBackend::new();

    let result = backend
        .probe_with_cache_config(&path, cache_config)
        .expect("configured DICOM probe");
    assert!(result.detected);
    let identity = FileIdentity::from_path(&path).unwrap();
    let probed_slide = backend
        .probe_cache
        .get(&identity, cache_config)
        .expect("probe retains the parsed slide for open");
    let image = &probed_slide.levels[0].parts[0];
    assert_eq!(
        image
            .encapsulated_frame_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capacity_entries(),
        0
    );
    assert_eq!(
        image
            .decoded_frame_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capacity_entries(),
        0
    );

    let reader = backend
        .open_with_cache_config(&path, cache_config)
        .expect("open consumes configured probe result");
    assert_eq!(Arc::strong_count(&probed_slide), 2);
    assert!(backend.probe_cache.get(&identity, cache_config).is_none());
    drop(reader);
    assert_eq!(Arc::strong_count(&probed_slide), 1);
}
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

fn assert_ybr_full_raw_compressed_frame_is_ycbcr(file_name: &str, transfer_syntax: &'static str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(file_name);
    let codestream = vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9];
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax,
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
        native_pixel_data: None,
        grid,
        pixel_spacing: None,
        objective_lens_power: None,
        encapsulated_frames: Mutex::new(None),
        encapsulated_frame_cache: Mutex::new(test_private_cache()),
        decoded_frame_cache: Mutex::new(test_private_cache()),
    })
}

fn test_private_cache<K: std::hash::Hash + Eq, V>() -> PrivateCache<K, V> {
    let mut budget = CacheConfig::deterministic()
        .with_shared_tile_bytes(4 * 1024)
        .private_cache_budget(1);
    PrivateCache::new(budget.allocate(1024))
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
fn encode_test_htj2k_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for index in 0..width * height {
        pixels.push(((index * 17 + index / 3) & 0xff) as u8);
        pixels.push(((index * 29 + 7) & 0xff) as u8);
        pixels.push(((index * 43 + 19) & 0xff) as u8);
    }
    let options = j2k_native::EncodeOptions {
        reversible: true,
        num_decomposition_levels: 1,
        ..j2k_native::EncodeOptions::default()
    };
    j2k_native::encode_htj2k(&pixels, width, height, 3, 8, false, &options)
        .expect("encode RGB HTJ2K fixture")
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
    reader_and_first_image_with_cache_config(path, CacheConfig::deterministic())
}

fn reader_and_first_image_with_cache_config(
    path: &Path,
    cache_config: CacheConfig,
) -> (DicomReader, Arc<DicomImage>) {
    let slide = Arc::new(
        DicomSlide::parse_with_cache_config(path, cache_config)
            .expect("parse generated DICOM slide"),
    );
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
fn compressed_dicom_default_cache_covers_common_read_region_working_set() {
    let levels = build_levels(
        Path::new("cache-hint.dcm"),
        vec![test_dicom_image_with_transfer_syntax(
            "1.2.3.1",
            DicomGrid::Full,
            uids::JPEG2000_LOSSLESS,
        )],
    )
    .expect("level should build");
    let working_set_bytes = levels[0]
        .cache_bytes_for_target_region()
        .expect("compressed DICOM working set should be computable");
    assert_eq!(working_set_bytes, 12 * 1024 * 1024);
    assert!(crate::core::cache::DEFAULT_TILE_CACHE_SIZE >= working_set_bytes);

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

type RecordedTileAdmissions = Arc<Mutex<Vec<Vec<(i64, i64)>>>>;

struct RecordingDicomReader {
    inner: DicomReader,
    controlled_admissions: RecordedTileAdmissions,
}

impl SlideReader for RecordingDicomReader {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.inner.tile_codec_kind(req)
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.inner.read_tiles(reqs, output)
    }

    fn read_tiles_controlled(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
        control: &crate::ReadControl,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.controlled_admissions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(reqs.iter().map(|req| (req.col, req.row)).collect());
        self.inner.read_tiles_controlled(reqs, output, control)
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_tile_cpu(req)
    }
}

#[test]
fn controlled_batch_of_eight_reaches_dicom_once_in_original_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-controlled-batch-eight.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 16 * 8;
    options.number_of_frames = 8;
    options.pixel_data = TestPixelData::EncapsulatedFrames(
        (0..8)
            .map(|index| encode_test_jpeg_rgb(16, 16, 3 + index * 19))
            .collect(),
    );
    write_test_dicom(&path, options);

    let (inner, _) = reader_and_first_image(&path);
    let controlled_admissions = Arc::new(Mutex::new(Vec::new()));
    let slide = Slide::from_source_with_cache_bytes(
        Box::new(RecordingDicomReader {
            inner,
            controlled_admissions: Arc::clone(&controlled_admissions),
        }),
        1024 * 1024,
    );
    let requests = [7, 0, 5, 2, 6, 1, 4, 3]
        .into_iter()
        .map(|col| tile_request(col, 0))
        .collect::<Vec<_>>();

    let tiles = slide
        .read_tiles_controlled(
            &requests,
            TileOutputPreference::cpu(),
            &crate::ReadControl::default(),
        )
        .expect("controlled DICOM batch of eight");

    assert_eq!(tiles.len(), requests.len());
    assert_eq!(
        *controlled_admissions
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        vec![requests
            .iter()
            .map(|request| (request.col, request.row))
            .collect::<Vec<_>>()],
        "the adaptive wrapper must admit one unchanged DICOM batch"
    );
    for (tile, expected) in tiles.iter().zip(requests.iter().map(|request| {
        slide
            .read_tile(request, TileOutputPreference::cpu())
            .expect("matching sequential tile")
    })) {
        let (TilePixels::Cpu(tile), TilePixels::Cpu(expected)) = (tile, expected) else {
            panic!("CPU output expected");
        };
        assert_eq!(tile.data.as_u8(), expected.data.as_u8());
    }
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
        .read_tiles_controlled(
            &[tile_request(1, 0), tile_request(0, 0)],
            TileOutputPreference::cpu(),
            &crate::ReadControl::default(),
        )
        .expect("read JP2K CPU tile batch");

    assert_eq!(tiles.len(), 2);
    assert!(tiles.iter().all(|tile| matches!(tile, TilePixels::Cpu(_))));
}

#[test]
fn controlled_jp2k_batch_preserves_edge_tile_dimensions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-controlled-edge-batch.dcm");
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

    let slide = Slide::open(&path).expect("open generated DICOM JP2K slide");
    let tiles = slide
        .read_tiles_controlled(
            &[tile_request(1, 0), tile_request(0, 0)],
            TileOutputPreference::cpu(),
            &crate::ReadControl::default(),
        )
        .expect("read controlled JP2K edge batch");

    let dimensions = tiles
        .into_iter()
        .map(|tile| match tile {
            TilePixels::Cpu(tile) => (tile.width, tile.height),
            TilePixels::Device(_) => panic!("CPU output expected"),
        })
        .collect::<Vec<_>>();
    assert_eq!(dimensions, vec![(8, 12), (16, 12)]);
}

#[test]
fn controlled_dicom_batch_cancelled_before_io_does_not_build_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-cancelled-before-io.dcm");
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

    let (reader, image) = reader_and_first_image(&path);
    let cancellation = crate::ReadCancellationToken::new();
    cancellation.cancel();
    let error = reader
        .read_tiles_controlled(
            &[tile_request(1, 0), tile_request(0, 0)],
            TileOutputPreference::cpu(),
            &crate::ReadControl::new(cancellation),
        )
        .expect_err("cancelled DICOM batch must stop before I/O");

    assert!(matches!(error, WsiError::Cancelled));
    assert!(
        image
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "cancellation before admission must not build the frame index"
    );
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

    let (reader, image) = reader_and_first_image_with_cache_config(
        &path,
        CacheConfig::deterministic().with_shared_tile_bytes(9 * 1024),
    );
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

#[test]
fn grouped_frame_read_validates_item_header_from_the_grouped_window() {
    struct CountingCursor {
        inner: std::io::Cursor<Vec<u8>>,
        read_calls: usize,
    }

    impl std::io::Read for CountingCursor {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.read_calls += 1;
            self.inner.read(buffer)
        }
    }

    impl std::io::Seek for CountingCursor {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("grouped-read-io-count.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let (_reader, image) = reader_and_first_image(&path);
    let payload = [1, 2, 3, 4];
    let fragment = DicomFragmentRef {
        item_offset: 0,
        payload_offset: 8,
        len: payload.len() as u32,
    };
    let frames = DicomEncapsulatedFrames {
        fragments: vec![fragment],
        frame_ranges: std::iter::once(0..1).collect(),
    };
    let group = DicomFrameReadGroup {
        start: 0,
        end: 12,
        spans: vec![DicomFrameReadSpan {
            frame_index: 0,
            frame_range: 0..1,
            start: 0,
            end: 12,
        }],
    };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&payload);
    let mut reader = CountingCursor {
        inner: std::io::Cursor::new(bytes),
        read_calls: 0,
    };

    let extracted = image
        .read_encapsulated_frame_group(&mut reader, &frames, &group)
        .expect("grouped read validates and extracts its frame");

    assert_eq!(extracted, vec![(0, payload.to_vec())]);
    assert_eq!(
        reader.read_calls, 1,
        "one grouped window read must provide both Item headers and payload bytes"
    );
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
fn controlled_indexing_reports_basic_offset_table_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-bot-diagnostics.dcm");
    let frames = [[0xFF, 0x4F, 0x01, 0x02], [0xFF, 0x4F, 0x03, 0x04]];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&8u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&12u32.to_le_bytes());
    for frame in &frames {
        push_pixel_fragment(&mut bytes, frame);
    }
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = crate::ReadControl::default().with_diagnostic_sink(Arc::new(
        move |event: crate::ReadDiagnostic| captured.lock().unwrap().push(event),
    ));

    let index = scan_encapsulated_frames_controlled(
        &path,
        uids::EXPLICIT_VR_LITTLE_ENDIAN,
        2,
        Some(&control),
    )
    .expect("fast indexing should resolve the nonzero BOT offset once");

    assert_eq!(index.frame_ranges, vec![0..1, 1..2]);
    let outcomes = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| match event {
            crate::ReadDiagnostic::DicomIndex(diagnostic) => diagnostic.outcome,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        vec![crate::DicomIndexOutcome::BuiltFast {
            mapping: crate::DicomIndexMapping::BasicOffsetTableItems,
        }]
    );
}

#[test]
fn raw_encapsulated_scan_uses_extended_offsets_for_multi_fragment_frames() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-eot-multi-fragment.dcm");
    let fragments = [[0xFF, 0x4F], [0x01, 0x02], [0xFF, 0x4F], [0x03, 0x04]];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let second_frame_offset = 2 * (8 + fragments[0].len() as u64);
    let mut eot = Vec::new();
    eot.extend_from_slice(&0u64.to_le_bytes());
    eot.extend_from_slice(&second_frame_offset.to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0xE0, 0x7F, 0x01, 0x00], b"OV", &eot);

    let mut eot_lengths = Vec::new();
    eot_lengths.extend_from_slice(&4u64.to_le_bytes());
    eot_lengths.extend_from_slice(&4u64.to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0xE0, 0x7F, 0x02, 0x00], b"OV", &eot_lengths);

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for fragment in &fragments {
        push_pixel_fragment(&mut bytes, fragment);
    }
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let frames = scan_encapsulated_frames_raw_little_endian(&path, 2)
        .expect("raw scan succeeds")
        .expect("Pixel Data is found");
    assert_eq!(frames.frame_ranges, vec![0..2, 2..4]);
}

#[test]
fn raw_encapsulated_scan_rejects_pixel_data_pattern_inside_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-false-pixel-data-pattern.dcm");
    let frame = [0xFF, 0x4F, 0x01, 0x02];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut metadata_value = Vec::new();
    metadata_value.extend_from_slice(&PIXEL_DATA_TAG_LE);
    metadata_value.extend_from_slice(b"OB");
    metadata_value.extend_from_slice(&[0, 0]);
    metadata_value.extend_from_slice(&UNDEFINED_LENGTH_LE);
    metadata_value.extend_from_slice(&[0; 16]);
    push_explicit_vr_long_element(&mut bytes, [0x11, 0x00, 0x10, 0x10], b"OB", &metadata_value);

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let item_offset = push_pixel_fragment(&mut bytes, &frame);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let frames = scan_encapsulated_frames_raw_little_endian(&path, 1)
        .expect("raw scan skips the false metadata candidate")
        .expect("real Pixel Data is found");
    assert_eq!(frames.frame_ranges, vec![0..1]);
    assert_eq!(frames.fragments[0].item_offset, item_offset);
}

#[test]
fn raw_encapsulated_scan_rejects_complete_pixel_sequence_inside_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-complete-false-pixel-sequence.dcm");
    let fake_frame = [0xDE, 0xAD, 0xBE, 0xEF];
    let real_frame = [0xFF, 0x4F, 0x01, 0x02];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut metadata_value = Vec::new();
    metadata_value.extend_from_slice(&PIXEL_DATA_TAG_LE);
    metadata_value.extend_from_slice(b"OB");
    metadata_value.extend_from_slice(&[0, 0]);
    metadata_value.extend_from_slice(&UNDEFINED_LENGTH_LE);
    metadata_value.extend_from_slice(&DICOM_ITEM_TAG_LE);
    metadata_value.extend_from_slice(&0u32.to_le_bytes());
    push_pixel_fragment(&mut metadata_value, &fake_frame);
    metadata_value.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    metadata_value.extend_from_slice(&0u32.to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0x11, 0x00, 0x10, 0x10], b"OB", &metadata_value);

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let real_item_offset = push_pixel_fragment(&mut bytes, &real_frame);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let frames = scan_encapsulated_frames_raw_little_endian(&path, 1)
        .expect("raw scan skips a complete false metadata sequence")
        .expect("real Pixel Data is found");

    assert_eq!(frames.frame_ranges, vec![0..1]);
    assert_eq!(frames.fragments[0].item_offset, real_item_offset);
}

#[test]
fn extended_offset_direct_path_rejects_invalid_intermediate_item_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-eot-invalid-middle-item.dcm");
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut offsets = Vec::new();
    for offset in [0u64, 12, 24] {
        offsets.extend_from_slice(&offset.to_le_bytes());
    }
    push_explicit_vr_long_element(&mut bytes, EXTENDED_OFFSET_TABLE_TAG_LE, b"OV", &offsets);
    let mut lengths = Vec::new();
    for length in [4u64, 4, 4] {
        lengths.extend_from_slice(&length.to_le_bytes());
    }
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE,
        b"OV",
        &lengths,
    );

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&[1, 2, 3, 4]);
    bytes.extend_from_slice(&[0xAA; 8]);
    bytes.extend_from_slice(&[5, 6, 7, 8]);
    push_pixel_fragment(&mut bytes, &[9, 10, 11, 12]);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    scan_encapsulated_frames_raw_little_endian(&path, 3)
        .expect_err("an EOT offset into payload bytes must not be accepted as an Item");
}

#[test]
fn extended_offset_table_values_are_bounds_checked_before_reading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-eot-values-out-of-bounds.dcm");
    std::fs::write(&path, vec![0u8; 64]).unwrap();
    let mut file = File::open(&path).unwrap();

    let error = read_extended_offset_tables_le(&mut file, &path, Some(56), Some(16), 16, None)
        .expect_err("EOT values beyond EOF must fail before allocation/read");

    assert!(error.to_string().contains("outside the source file"));
}

#[test]
fn extended_fragment_padding_rejects_u32_overflow() {
    let error = checked_padded_fragment_len(
        Path::new("overflowing-extended-length.dcm"),
        0,
        u64::from(u32::MAX),
    )
    .expect_err("odd u32::MAX payload length cannot be represented after padding");

    assert!(error.to_string().contains("padded length"));
}

#[test]
fn malformed_extended_offsets_fall_back_to_valid_basic_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-invalid-eot-valid-bot.dcm");
    let frames = [[0xFF, 0x4F, 0x01, 0x02], [0xFF, 0x4F, 0x03, 0x04]];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut invalid_eot = Vec::new();
    invalid_eot.extend_from_slice(&0u64.to_le_bytes());
    invalid_eot.extend_from_slice(&1u64.to_le_bytes());
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_TAG_LE,
        b"OV",
        &invalid_eot,
    );
    let mut lengths = Vec::new();
    lengths.extend_from_slice(&4u64.to_le_bytes());
    lengths.extend_from_slice(&4u64.to_le_bytes());
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE,
        b"OV",
        &lengths,
    );

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&8u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(8u32 + frames[0].len() as u32).to_le_bytes());
    for frame in &frames {
        push_pixel_fragment(&mut bytes, frame);
    }
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let index = scan_encapsulated_frames_raw_little_endian(&path, 2)
        .expect("valid BOT safely replaces malformed EOT")
        .expect("Pixel Data is found");
    assert_eq!(index.frame_ranges, vec![0..1, 1..2]);
}

#[test]
fn malformed_extended_offsets_without_safe_mapping_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-invalid-eot-no-fallback.dcm");
    let fragments = [[1, 2], [3, 4], [5, 6], [7, 8]];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut invalid_eot = Vec::new();
    invalid_eot.extend_from_slice(&0u64.to_le_bytes());
    invalid_eot.extend_from_slice(&1u64.to_le_bytes());
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_TAG_LE,
        b"OV",
        &invalid_eot,
    );
    let mut lengths = Vec::new();
    lengths.extend_from_slice(&4u64.to_le_bytes());
    lengths.extend_from_slice(&4u64.to_le_bytes());
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE,
        b"OV",
        &lengths,
    );
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for fragment in &fragments {
        push_pixel_fragment(&mut bytes, fragment);
    }
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let error = scan_encapsulated_frames_raw_little_endian(&path, 2)
        .expect_err("malformed EOT without BOT/item mapping must fail");
    assert!(error.to_string().contains("extended offset table"));
}

#[test]
fn raw_encapsulated_scan_rejects_fragment_extending_past_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-truncated-fragment.dcm");
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&1024u32.to_le_bytes());
    bytes.extend_from_slice(&[1, 2, 3, 4]);
    std::fs::write(&path, bytes).unwrap();

    let error = scan_encapsulated_frames_raw_little_endian(&path, 1)
        .expect_err("truncated fragment must fail safely");
    assert!(error.to_string().contains("beyond the source file"));
}

#[test]
fn raw_item_scan_seeks_over_fragment_payloads() {
    struct CountingCursor {
        inner: std::io::Cursor<Vec<u8>>,
        bytes_read: usize,
    }

    impl std::io::Read for CountingCursor {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes_read += read;
            Ok(read)
        }
    }

    impl std::io::Seek for CountingCursor {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let payload_len = 1024 * 1024u32;
    let mut bytes = Vec::with_capacity(payload_len as usize + 36);
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.resize(bytes.len() + payload_len as usize, 0xA5);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let file_len = bytes.len() as u64;
    let mut reader = CountingCursor {
        inner: std::io::Cursor::new(bytes),
        bytes_read: 0,
    };

    let (fragments, basic_offsets) = scan_raw_encapsulated_pixel_sequence_with_reader(
        &mut reader,
        Path::new("counted-payload.dcm"),
        0,
        file_len,
        None,
    )
    .expect("item scan succeeds");

    assert_eq!(fragments.len(), 1);
    assert!(basic_offsets.is_empty());
    assert_eq!(
        reader.bytes_read, 24,
        "indexing should read only the BOT, fragment, and delimiter headers"
    );
}

#[test]
fn oversized_basic_offset_table_is_rejected_without_allocating_its_payload() {
    let error = validate_basic_offset_table_len(
        Path::new("oversized-basic-offset-table.dcm"),
        u32::MAX - 3,
        None,
    )
    .expect_err("an untrusted multi-gigabyte basic offset table must be rejected");

    assert!(
        error.to_string().contains("exceeds safety limit"),
        "unexpected error: {error}"
    );
}

#[test]
fn compressed_frame_preflight_enforces_exact_limit_for_every_fragment() {
    let path = Path::new("compressed-frame-limit.dcm");
    let exact = DicomFragmentRef {
        item_offset: 0,
        payload_offset: 8,
        len: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES as u32,
    };
    let total_len = preflight_compressed_frame(path, &[exact])
        .expect("the exact compressed-frame limit must be accepted");
    assert_eq!(
        total_len,
        crate::core::limits::MAX_COMPRESSED_INPUT_BYTES as usize
    );

    for fragments in [
        vec![DicomFragmentRef {
            item_offset: 0,
            payload_offset: 8,
            len: (crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 1) as u32,
        }],
        vec![
            DicomFragmentRef {
                item_offset: 0,
                payload_offset: 8,
                len: 1,
            },
            DicomFragmentRef {
                item_offset: 9,
                payload_offset: 17,
                len: (crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 1) as u32,
            },
        ],
        vec![
            DicomFragmentRef {
                item_offset: 0,
                payload_offset: 8,
                len: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES as u32,
            },
            DicomFragmentRef {
                item_offset: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 8,
                payload_offset: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 16,
                len: 1,
            },
        ],
    ] {
        let error = preflight_compressed_frame(path, &fragments)
            .expect_err("over-limit frame must be rejected before allocation");
        assert!(
            matches!(error, WsiError::ResourceLimit { .. }),
            "expected typed resource limit, got {error:?}"
        );
    }
}

#[test]
fn compressed_frame_preflight_rejects_offset_arithmetic_overflow() {
    let error = preflight_compressed_frame(
        Path::new("compressed-frame-overflow.dcm"),
        &[DicomFragmentRef {
            item_offset: u64::MAX - 9,
            payload_offset: u64::MAX - 1,
            len: 4,
        }],
    )
    .expect_err("fragment end overflow must fail before any read or allocation");

    assert!(error.to_string().contains("offset overflow"), "{error}");
}

#[test]
fn raw_item_scan_rejects_oversized_basic_offset_table_before_reading_payload() {
    struct CountingCursor {
        inner: std::io::Cursor<Vec<u8>>,
        bytes_read: usize,
    }

    impl std::io::Read for CountingCursor {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes_read += read;
            Ok(read)
        }
    }

    impl std::io::Seek for CountingCursor {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let declared_len = u32::MAX - 3;
    let mut bytes = vec![0; EXPLICIT_VR_LONG_HEADER_LEN];
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&declared_len.to_le_bytes());
    let file_len = bytes.len() as u64 + u64::from(declared_len);
    let mut reader = CountingCursor {
        inner: std::io::Cursor::new(bytes),
        bytes_read: 0,
    };

    let error = scan_raw_encapsulated_pixel_sequence_with_reader(
        &mut reader,
        Path::new("oversized-basic-offset-table.dcm"),
        0,
        file_len,
        None,
    )
    .expect_err("the scanner must reject an oversized basic offset table");

    assert!(error.to_string().contains("exceeds safety limit"));
    assert_eq!(
        reader.bytes_read, 8,
        "only the basic offset table Item header may be read"
    );
}

#[test]
fn cancellation_during_basic_offset_table_read_stops_before_next_chunk() {
    struct CancellingTableReader {
        inner: std::io::Cursor<Vec<u8>>,
        cancellation: crate::ReadCancellationToken,
        bytes_read: usize,
    }

    impl std::io::Read for CancellingTableReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let reading_table_payload = self.inner.position()
                >= u64::try_from(EXPLICIT_VR_LONG_HEADER_LEN + 8).expect("header offset");
            let read = self.inner.read(buffer)?;
            self.bytes_read += read;
            if reading_table_payload && read > 0 {
                self.cancellation.cancel();
            }
            Ok(read)
        }
    }

    impl std::io::Seek for CancellingTableReader {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let table_len = 128 * 1024u32;
    let mut bytes = vec![0; EXPLICIT_VR_LONG_HEADER_LEN];
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&table_len.to_le_bytes());
    bytes.resize(bytes.len() + table_len as usize, 0);
    let file_len = bytes.len() as u64;
    let cancellation = crate::ReadCancellationToken::new();
    let control = crate::ReadControl::new(cancellation.clone());
    let mut reader = CancellingTableReader {
        inner: std::io::Cursor::new(bytes),
        cancellation,
        bytes_read: 0,
    };

    let error = scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
        &mut reader,
        Path::new("cancelled-basic-offset-table.dcm"),
        0,
        file_len,
        Some(table_len / 4),
        Some(&control),
    )
    .expect_err("cancellation after the first table chunk must stop the scan");

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(
        reader.bytes_read,
        8 + 64 * 1024,
        "the second table chunk must not be admitted"
    );
}

#[test]
fn raw_item_scan_cancellation_stops_before_the_next_header_admission() {
    struct CancellingCursor {
        inner: std::io::Cursor<Vec<u8>>,
        token: crate::ReadCancellationToken,
        bytes_read: usize,
    }

    impl std::io::Read for CancellingCursor {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes_read += read;
            if read > 0 {
                self.token.cancel();
            }
            Ok(read)
        }
    }

    impl std::io::Seek for CancellingCursor {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    push_pixel_fragment(&mut bytes, &[1, 2, 3, 4]);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let file_len = bytes.len() as u64;
    let token = crate::ReadCancellationToken::new();
    let control = crate::ReadControl::new(token.clone());
    let mut reader = CancellingCursor {
        inner: std::io::Cursor::new(bytes),
        token,
        bytes_read: 0,
    };

    let error = scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
        &mut reader,
        Path::new("cancelled-item-scan.dcm"),
        0,
        file_len,
        Some(1),
        Some(&control),
    )
    .expect_err("cancellation must stop before the fragment header is admitted");

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(reader.bytes_read, 8, "only the BOT header should be read");
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
fn basic_offset_table_maps_a_nonzero_second_frame_offset_once() {
    let frames = build_encapsulated_frame_index(
        Path::new("two-frame-basic-offset-table.dcm"),
        vec![
            DicomFragmentRef {
                payload_offset: 108,
                item_offset: 100,
                len: 4,
            },
            DicomFragmentRef {
                payload_offset: 120,
                item_offset: 112,
                len: 4,
            },
        ],
        vec![0, 12],
        2,
    )
    .expect("the BOT offset is relative to the first fragment Item exactly once");

    assert_eq!(frames.frame_ranges, vec![0..1, 1..2]);
}

#[test]
fn extended_offset_validation_rejects_non_monotonic_and_overflowing_offsets() {
    let path = Path::new("malformed-extended-offsets.dcm");
    let fragments = vec![
        DicomFragmentRef {
            payload_offset: 108,
            item_offset: 100,
            len: 4,
        },
        DicomFragmentRef {
            payload_offset: 120,
            item_offset: 112,
            len: 4,
        },
    ];
    let non_monotonic = DicomExtendedOffsetTables {
        offsets: vec![0, 0],
        lengths: vec![4, 4],
    };
    let error = frame_ranges_from_extended_offsets(path, &fragments, &non_monotonic, 2)
        .expect_err("non-monotonic EOT must fail");
    assert!(error.to_string().contains("strictly increasing"));

    let overflowing = DicomExtendedOffsetTables {
        offsets: vec![0, u64::MAX],
        lengths: vec![4, 4],
    };
    let error = frame_ranges_from_extended_offsets(path, &fragments, &overflowing, 2)
        .expect_err("overflowing EOT must fail");
    assert!(error.to_string().contains("overflow"));
}

#[test]
#[cfg(feature = "metal")]
fn local_htj2k_dicom_full_tile_can_require_device_output() {
    let Some(path) = local_htj2k_dicom_fixture() else {
        return;
    };
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping local HTJ2K DICOM device test; no Metal device");
        return;
    };

    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let tile = slide
        .read_tile_controlled(
            &TileRequest {
                scene: 0usize.into(),
                series: 0usize.into(),
                level: 0u32.into(),
                plane: PlaneSelection::default().into(),
                col: 0,
                row: 0,
            },
            TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions),
            &crate::ReadControl::default(),
        )
        .expect("read full HTJ2K tile with required device output");

    assert!(matches!(tile, TilePixels::Device(_)));
}

#[test]
#[cfg(feature = "metal")]
fn controlled_classic_jp2k_and_htj2k_keep_metal_output() {
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping controlled JP2K residency test; no Metal device");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let classic = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let htj2k = encode_test_htj2k_rgb(16, 12);

    for (name, transfer_syntax, codestream) in [
        ("classic", uids::JPEG2000_LOSSLESS, classic),
        ("htj2k", HTJ2K_LOSSLESS_TRANSFER_SYNTAX, htj2k),
    ] {
        let path = dir.path().join(format!("controlled-{name}.dcm"));
        let mut options = TestDicomOptions::native(Vec::new());
        options.transfer_syntax = transfer_syntax;
        options.rows = 12;
        options.columns = 16;
        options.total_pixel_matrix_rows = 12;
        options.total_pixel_matrix_columns = 16;
        options.pixel_data = TestPixelData::EncapsulatedFrames(vec![codestream]);
        write_test_dicom(&path, options);
        let slide = Slide::open(&path).expect("open generated JP2K DICOM");

        let tile = slide
            .read_tile_controlled(
                &tile_request(0, 0),
                TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(
                    sessions.clone(),
                ),
                &crate::ReadControl::default(),
            )
            .unwrap_or_else(|error| panic!("controlled {name} device decode failed: {error}"));

        assert!(
            matches!(tile, TilePixels::Device(DeviceTile::Metal(_))),
            "controlled {name} decode must remain Metal-resident"
        );
    }
}

#[test]
#[cfg(feature = "metal")]
fn local_htj2k_dicom_prefer_device_batch_keeps_full_tiles_on_device() {
    let Some(path) = local_htj2k_dicom_fixture() else {
        return;
    };
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping local HTJ2K DICOM device test; no Metal device");
        return;
    };

    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let tiles = slide
        .read_tiles_controlled(
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
            &crate::ReadControl::default(),
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
#[cfg(feature = "parity-metal")]
fn local_htj2k_dicom_full_tile_pixels_match_cpu_on_metal() {
    let Some(path) = local_htj2k_dicom_fixture() else {
        return;
    };
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping local HTJ2K DICOM parity test; no Metal device");
        return;
    };

    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let level = &slide.dataset().scenes[0].series[0].levels[0];
    let TileLayout::Regular {
        tile_width,
        tile_height,
        ..
    } = level.tile_layout
    else {
        panic!("local HTJ2K DICOM fixture must use a regular tile grid");
    };
    assert!(level.dimensions.0 >= u64::from(tile_width));
    assert!(level.dimensions.1 >= u64::from(tile_height));
    let requests = [TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    }];
    let cpu = slide
        .read_tiles_controlled(
            &requests,
            TileOutputPreference::cpu(),
            &crate::ReadControl::default(),
        )
        .expect("read CPU parity tiles");
    let device = slide
        .read_tiles_controlled(
            &requests,
            TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions)
                .without_adaptive_decode_route(),
            &crate::ReadControl::default(),
        )
        .expect("read Metal parity tiles");

    for (index, (cpu, device)) in cpu.into_iter().zip(device).enumerate() {
        let TilePixels::Cpu(cpu) = cpu else {
            panic!("CPU parity request {index} returned device pixels");
        };
        let TilePixels::Device(DeviceTile::Metal(device)) = device else {
            panic!("Metal parity request {index} returned CPU pixels");
        };
        let resident = device
            .validated_resident_image()
            .expect("validated resident Metal tile");
        let metal = crate::output::metal::resident_bytes(resident);
        let cpu = cpu.data.as_u8().expect("CPU parity tile is RGB8");
        assert_eq!(metal.len(), cpu.len(), "tile {index} byte cardinality");
        let max_delta = metal
            .iter()
            .zip(cpu)
            .map(|(metal, cpu)| metal.abs_diff(*cpu))
            .max()
            .unwrap_or(0);
        assert!(max_delta <= 4, "tile {index} max channel delta {max_delta}");
    }
}

#[test]
fn local_htj2k_dicom_level_preparation_meets_interactive_budget() {
    let Some(path) = local_htj2k_dicom_fixture() else {
        return;
    };
    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let started = std::time::Instant::now();
    slide
        .prepare_level_controlled(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(0),
            &crate::ReadControl::default(),
        )
        .expect("prepare local HTJ2K DICOM base level");
    let elapsed = started.elapsed();
    eprintln!("local HTJ2K DICOM level preparation: {elapsed:?}");
    assert!(
        elapsed < std::time::Duration::from_millis(75),
        "DICOM level preparation should remain inside the 75 ms interactive budget"
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

fn local_htj2k_dicom_fixture() -> Option<PathBuf> {
    let Some(path) = std::env::var_os("WSI_RS_LOCAL_HTJ2K_DICOM").map(PathBuf::from) else {
        eprintln!("skipping local HTJ2K DICOM device test; WSI_RS_LOCAL_HTJ2K_DICOM unset");
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
    assert_ybr_full_raw_compressed_frame_is_ycbcr(
        "htj2k-rpcl-ybr-full.dcm",
        HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
    );
}

#[test]
fn reads_general_htj2k_ybr_full_raw_compressed_frame_as_ycbcr() {
    assert_ybr_full_raw_compressed_frame_is_ycbcr(
        "htj2k-general-ybr-full.dcm",
        "1.2.840.10008.1.2.4.203",
    );
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
fn prepare_level_controlled_builds_the_lazy_frame_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prepare-level.dcm");
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
    let slide = Arc::new(DicomSlide::parse(&path).expect("parse DICOM slide"));
    let image = slide.levels[0].parts[0].clone();
    let reader = DicomReader { slide };
    let handle = Slide::from_source_with_cache_bytes(Box::new(reader), 1024 * 1024);

    handle
        .prepare_level_controlled(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(0),
            &crate::ReadControl::default(),
        )
        .expect("prepare DICOM level");

    assert!(
        image
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_some(),
        "preparation should publish the complete frame index"
    );
}

#[test]
fn controlled_preparation_reports_fast_index_build_then_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prepare-level-diagnostics.dcm");
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
    let slide = Arc::new(DicomSlide::parse(&path).expect("parse DICOM slide"));
    let reader = DicomReader { slide };
    let handle = Slide::from_source_with_cache_bytes(Box::new(reader), 1024 * 1024);
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = crate::ReadControl::default().with_diagnostic_sink(Arc::new(
        move |event: crate::ReadDiagnostic| captured.lock().unwrap().push(event),
    ));

    for _ in 0..2 {
        handle
            .prepare_level_controlled(
                SceneId::new(0),
                SeriesId::new(0),
                LevelIdx::new(0),
                &control,
            )
            .expect("prepare DICOM level");
    }

    let outcomes = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| match event {
            crate::ReadDiagnostic::DicomIndex(diagnostic) => diagnostic.outcome,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        vec![
            crate::DicomIndexOutcome::BuiltFast {
                mapping: crate::DicomIndexMapping::SingleFrameItems,
            },
            crate::DicomIndexOutcome::Reused,
        ]
    );
}

#[test]
fn controlled_preparation_invokes_diagnostic_sink_after_releasing_index_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prepare-level-reentrant-diagnostics.dcm");
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
    let slide = Arc::new(DicomSlide::parse(&path).expect("parse DICOM slide"));
    let image = slide.levels[0].parts[0].clone();
    let reader = DicomReader { slide };
    let handle = Slide::from_source_with_cache_bytes(Box::new(reader), 1024 * 1024);
    let callback_observed_unlocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = Arc::clone(&callback_observed_unlocked);
    let callback_image = image.clone();
    let control = crate::ReadControl::default().with_diagnostic_sink(Arc::new(
        move |_event: crate::ReadDiagnostic| {
            let lock_available = callback_image.encapsulated_frames.try_lock().is_ok();
            observed.store(lock_available, std::sync::atomic::Ordering::Release);
        },
    ));

    handle
        .prepare_level_controlled(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(0),
            &control,
        )
        .expect("prepare DICOM level");

    assert!(
        callback_observed_unlocked.load(std::sync::atomic::Ordering::Acquire),
        "diagnostic callbacks must not run while the encapsulated-frame index mutex is held"
    );
}

#[test]
fn controlled_indexing_reports_token_fallback_for_implicit_vr_layout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("implicit-vr-index-fallback.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: uids::IMPLICIT_VR_LITTLE_ENDIAN,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = crate::ReadControl::default().with_diagnostic_sink(Arc::new(
        move |event: crate::ReadDiagnostic| captured.lock().unwrap().push(event),
    ));

    let frames = scan_encapsulated_frames_controlled(
        &path,
        uids::IMPLICIT_VR_LITTLE_ENDIAN,
        1,
        Some(&control),
    )
    .expect("token parser should index the implicit-VR encapsulated layout");

    assert_eq!(frames.frame_ranges, vec![0..1]);
    let outcomes = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| match event {
            crate::ReadDiagnostic::DicomIndex(diagnostic) => diagnostic.outcome,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        vec![
            crate::DicomIndexOutcome::FastPathFallback,
            crate::DicomIndexOutcome::TokenFallback,
        ]
    );
}

#[test]
fn disabled_index_diagnostics_do_not_sample_the_clock() {
    let clock_calls = std::cell::Cell::new(0);
    let started = index_diagnostic_timer_with(Some(&crate::ReadControl::default()), false, || {
        clock_calls.set(clock_calls.get() + 1);
        std::time::Instant::now()
    });

    assert!(started.is_none());
    assert_eq!(clock_calls.get(), 0);
}

#[test]
fn concurrent_frame_index_preparation_reuses_one_complete_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent-prepare-level.dcm");
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
    let image = slide.levels[0].parts[0].clone();
    let workers = (0..8)
        .map(|_| {
            let image = image.clone();
            std::thread::spawn(move || image.ensure_encapsulated_frames().unwrap())
        })
        .collect::<Vec<_>>();
    let indexes = workers
        .into_iter()
        .map(|worker| worker.join().expect("preparation worker did not panic"))
        .collect::<Vec<_>>();

    assert!(
        indexes
            .windows(2)
            .all(|pair| Arc::ptr_eq(&pair[0], &pair[1])),
        "concurrent preparation should publish and reuse one complete index"
    );
}

#[test]
fn cancelled_level_preparation_does_not_publish_an_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel-prepare-level.dcm");
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
    let slide = Arc::new(DicomSlide::parse(&path).expect("parse DICOM slide"));
    let image = slide.levels[0].parts[0].clone();
    let reader = DicomReader { slide };
    let cancellation = crate::ReadCancellationToken::new();
    cancellation.cancel();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = crate::ReadControl::new(cancellation).with_diagnostic_sink(Arc::new(
        move |event: crate::ReadDiagnostic| captured.lock().unwrap().push(event),
    ));

    let error = reader
        .prepare_level_controlled(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(0),
            &control,
        )
        .expect_err("cancelled preparation should stop");

    assert!(matches!(error, WsiError::Cancelled));
    assert!(
        events.lock().unwrap().is_empty(),
        "cancelled preparation must not report an index outcome"
    );
    assert!(
        image
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "cancelled preparation must not publish a partial index"
    );
}

#[test]
fn cancellation_during_frame_index_build_does_not_publish_the_completed_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel-during-index-build.dcm");
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
    let image = slide.levels[0].parts[0].clone();
    let cancellation = crate::ReadCancellationToken::new();
    let control = crate::ReadControl::new(cancellation.clone());

    let error = image
        .ensure_encapsulated_frames_with_builder(Some(&control), || {
            cancellation.cancel();
            Ok(DicomEncapsulatedFrames {
                fragments: vec![DicomFragmentRef {
                    item_offset: 0,
                    payload_offset: 8,
                    len: 4,
                }],
                frame_ranges: std::iter::once(0..1).collect(),
            })
        })
        .expect_err("a cancelled build must not publish its completed candidate");

    assert!(matches!(error, WsiError::Cancelled));
    assert!(
        image
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "cancellation during the build must leave no cached index"
    );
}

#[test]
fn cancellation_during_extended_table_read_does_not_publish_an_index() {
    struct CancellingTableReader {
        inner: std::io::Cursor<Vec<u8>>,
        cancellation: crate::ReadCancellationToken,
    }

    impl std::io::Read for CancellingTableReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            if read > 0 {
                self.cancellation.cancel();
            }
            Ok(read)
        }
    }

    impl std::io::Seek for CancellingTableReader {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel-during-extended-table.dcm");
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
    let image = slide.levels[0].parts[0].clone();
    let cancellation = crate::ReadCancellationToken::new();
    let control = crate::ReadControl::new(cancellation.clone());
    let table_len = 64 * 1024u32;
    let mut reader = CancellingTableReader {
        inner: std::io::Cursor::new(vec![0; 2 * table_len as usize]),
        cancellation,
    };

    let error = image
        .ensure_encapsulated_frames_with_builder(Some(&control), || {
            let _ = read_extended_offset_tables_with_reader(
                &mut reader,
                &path,
                0,
                u64::from(table_len),
                table_len,
                2 * u64::from(table_len),
                Some(&control),
            )?;
            Ok(DicomEncapsulatedFrames {
                fragments: Vec::new(),
                frame_ranges: Vec::new(),
            })
        })
        .expect_err("cancellation between bounded table chunks must stop preparation");

    assert!(matches!(error, WsiError::Cancelled));
    assert!(
        image
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "a cancelled extended-table read must not publish an index"
    );
}

#[test]
fn indexed_fragment_header_is_revalidated_before_payload_read() {
    use std::io::{Seek as _, Write as _};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fragment-header-revalidation.dcm");
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
    let image = slide.levels[0].parts[0].clone();
    let frames = image
        .ensure_encapsulated_frames()
        .expect("build the frame index before corrupting the source");
    let item_offset = frames.fragments[0].item_offset;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open fixture for corruption");
    file.seek(std::io::SeekFrom::Start(item_offset))
        .expect("seek to fragment Item header");
    file.write_all(&[0xAA; 4])
        .expect("replace fragment Item tag");
    drop(file);

    let error = image
        .extract_encapsulated_frame(0, 0, 0, 0, false)
        .expect_err("an indexed fragment with a corrupt Item header must not be returned");

    assert!(error
        .to_string()
        .contains("does not match its indexed length"));
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
#[cfg(feature = "metal")]
fn mixed_device_batch_admits_one_ordered_cpu_remainder_batch() {
    fn marker_tile(value: u8) -> CpuTile {
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![value, 0, 0]).unwrap()
    }

    let requests = [0, 1, 2, 3]
        .into_iter()
        .map(|col| tile_request(col, 0))
        .collect::<Vec<_>>();
    let results = vec![
        Some(TilePixels::Cpu(marker_tile(10))),
        None,
        Some(TilePixels::Cpu(marker_tile(30))),
        None,
    ];
    let codec_admissions = std::cell::RefCell::new(Vec::new());

    let completed = complete_mixed_device_batch_with_cpu_remainder(
        &requests,
        &TileOutputPreference::prefer_device_auto_with_compressed_decode(),
        BackendRequest::Auto,
        results,
        None,
        |remainder, _, _| {
            codec_admissions.borrow_mut().push(
                remainder
                    .iter()
                    .map(|request| request.col)
                    .collect::<Vec<_>>(),
            );
            Ok(vec![marker_tile(20), marker_tile(40)])
        },
    )
    .expect("complete mixed device/CPU batch");

    assert_eq!(*codec_admissions.borrow(), vec![vec![1, 3]]);
    assert_eq!(completed.len(), requests.len());
    assert_eq!(
        completed
            .iter()
            .map(|tile| match tile {
                TilePixels::Cpu(tile) => tile.data.as_u8().unwrap()[0],
                TilePixels::Device(_) => panic!("synthetic completion uses CPU marker tiles"),
            })
            .collect::<Vec<_>>(),
        vec![10, 20, 30, 40],
        "CPU remainder results must return to their original request slots"
    );
}

#[test]
#[cfg(feature = "metal")]
fn cancelled_mixed_device_batch_never_admits_a_cpu_remainder() {
    let token = crate::ReadCancellationToken::new();
    token.cancel();
    let control = crate::ReadControl::new(token);
    let admissions = std::cell::Cell::new(0_usize);

    let error = complete_mixed_device_batch_with_cpu_remainder(
        &[tile_request(0, 0)],
        &TileOutputPreference::prefer_device_auto_with_compressed_decode(),
        BackendRequest::Auto,
        vec![None],
        Some(&control),
        |_, _, _| {
            admissions.set(admissions.get() + 1);
            Ok(Vec::new())
        },
    )
    .expect_err("cancelled mixed batch must not enter CPU fallback");

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(admissions.get(), 0);
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
