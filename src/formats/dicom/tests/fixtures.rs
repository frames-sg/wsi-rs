use super::*;
pub(super) enum TestPixelData {
    Native(Vec<u8>),
    Encapsulated(Vec<u8>),
    EncapsulatedFrames(Vec<Vec<u8>>),
}

pub(super) struct TestOpticalPathIccProfile {
    pub(super) optical_path_identifier: Option<&'static str>,
    pub(super) bytes: Vec<u8>,
}
pub(super) struct TestDicomOptions {
    pub(super) sop_instance_uid: &'static str,
    pub(super) series_instance_uid: &'static str,
    pub(super) image_type: &'static str,
    pub(super) transfer_syntax: &'static str,
    pub(super) samples_per_pixel: u16,
    pub(super) photometric_interpretation: &'static str,
    pub(super) planar_configuration: Option<u16>,
    pub(super) rows: u16,
    pub(super) columns: u16,
    pub(super) total_pixel_matrix_rows: u32,
    pub(super) total_pixel_matrix_columns: u32,
    pub(super) number_of_frames: u32,
    pub(super) pixel_spacing: Option<&'static str>,
    pub(super) shared_pixel_spacing: Option<&'static str>,
    pub(super) barcode_value: Option<&'static str>,
    pub(super) optical_path_icc_profiles: Vec<TestOpticalPathIccProfile>,
    pub(super) pixel_data: TestPixelData,
}

impl TestDicomOptions {
    pub(super) fn native(pixel_data: Vec<u8>) -> Self {
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
            barcode_value: None,
            optical_path_icc_profiles: Vec::new(),
            pixel_data: TestPixelData::Native(pixel_data),
        }
    }
}

pub(super) fn test_rgb_pixel_data() -> Vec<u8> {
    vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]
}

pub(super) fn write_test_dicom(path: &Path, options: TestDicomOptions) {
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
    if let Some(barcode_value) = options.barcode_value {
        object.put(DataElement::new(tags::BARCODE_VALUE, VR::LT, barcode_value));
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

pub(super) fn encode_test_jpeg_rgb(width: u16, height: u16, seed: u8) -> Vec<u8> {
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

pub(super) fn extended_sequential_8x8_jpeg() -> Vec<u8> {
    let mut jpeg = encode_test_jpeg_rgb(8, 8, 17);
    let sof = jpeg
        .windows(2)
        .position(|marker| marker == [0xff, 0xc0])
        .expect("baseline fixture contains SOF0");
    jpeg[sof + 1] = 0xc1;
    jpeg
}

fn literal_rle_segment(bytes: &[u8]) -> Vec<u8> {
    assert!((1..=128).contains(&bytes.len()));
    let mut encoded = Vec::with_capacity(bytes.len() + 1);
    encoded.push((bytes.len() - 1) as u8);
    encoded.extend_from_slice(bytes);
    encoded
}

pub(super) fn rle_rgb_frame(r: &[u8], g: &[u8], b: &[u8]) -> Vec<u8> {
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
