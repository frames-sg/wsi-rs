use super::fixtures::*;
use super::*;
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

fn rgb_bytes(tile: &CpuTile) -> Vec<u8> {
    assert_eq!(tile.width, 2);
    assert_eq!(tile.height, 2);
    assert_eq!(tile.channels, 3);
    assert_eq!(tile.color_space, ColorSpace::Rgb);
    assert_eq!(tile.layout, CpuTileLayout::Interleaved);
    tile.data.as_u8().expect("u8 RGB tile").to_vec()
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

#[test]
fn corrupt_compressed_frames_preserve_tile_context_for_each_codec() {
    for (file_name, transfer_syntax, frame) in [
        ("corrupt-jpeg.dcm", JPEG_TRANSFER_SYNTAX, vec![0; 16]),
        ("corrupt-jp2k.dcm", uids::JPEG2000_LOSSLESS, vec![0; 16]),
        ("corrupt-rle.dcm", RLE_TRANSFER_SYNTAX, vec![0; 16]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file_name);
        let mut options = TestDicomOptions::native(Vec::new());
        options.transfer_syntax = transfer_syntax;
        options.pixel_data = TestPixelData::Encapsulated(frame);
        write_test_dicom(&path, options);
        let slide = Slide::open(&path).expect("corrupt payload metadata should still open");
        let request = TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        };

        let error = match slide.read_tile(&request, TileOutputPreference::cpu()) {
            Ok(_) => panic!("corrupt {transfer_syntax} frame must fail decoding"),
            Err(error) => error,
        };
        let WsiError::TileRead {
            col,
            row,
            level,
            reason,
        } = error
        else {
            panic!("expected contextual tile-read error, got {error:?}");
        };
        assert_eq!((col, row, level), (0, 0, 0));
        assert!(!reason.is_empty());

        let (reader, _) = super::runtime::reader_and_first_image(&path);
        let direct_error = match reader.read_tile_cpu(&request) {
            Ok(_) => panic!("direct corrupt {transfer_syntax} frame decode must fail"),
            Err(error) => error,
        };
        let WsiError::TileRead {
            col,
            row,
            level,
            reason,
        } = direct_error
        else {
            panic!("expected direct contextual tile-read error, got {direct_error:?}");
        };
        assert_eq!((col, row, level), (0, 0, 0));
        assert!(!reason.is_empty());
    }
}
