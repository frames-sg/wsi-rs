use super::*;
pub(super) fn test_dicom_image_with_transfer_syntax(
    sop_instance_uid: &str,
    grid: DicomGrid,
    transfer_syntax_uid: &str,
) -> Arc<DicomImage> {
    Arc::new(DicomImage {
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
        icc_profile: Vec::new(),
        frame_store: DicomFrameStore {
            path: PathBuf::from(format!("{sop_instance_uid}.dcm")),
            encoded_unit_bytes: crate::SlideLimits::default().encoded_unit_bytes(),
            native_pixel_data: None,
            encapsulated_frames: Mutex::new(None),
            compressed_frame_cache: Mutex::new(test_private_cache()),
        },
        decoded_frame_cache: Mutex::new(test_private_cache()),
    })
}

fn test_private_cache<K: std::hash::Hash + Eq, V>() -> PrivateCache<K, V> {
    let mut budget = CacheConfig::deterministic()
        .with_shared_tile_bytes(4 * 1024)
        .private_cache_budget(1);
    PrivateCache::new(budget.allocate(1024))
}

pub(super) fn empty_dataset() -> Dataset {
    Dataset {
        id: DatasetId::new(1),
        scenes: Vec::new(),
        associated_images: HashMap::new(),
        properties: Properties::new(),
        icc_profiles: HashMap::new(),
        source_icc_profiles: Vec::new(),
    }
}

pub(super) fn tile_request(col: i64, row: i64) -> TileRequest {
    TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col,
        row,
    }
}

pub(super) fn reader_and_first_image(path: &Path) -> (DicomReader, Arc<DicomImage>) {
    reader_and_first_image_with_cache_config(path, CacheConfig::deterministic())
}

pub(super) fn reader_and_first_image_with_cache_config(
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
