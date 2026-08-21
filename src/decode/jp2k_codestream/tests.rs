use super::*;

fn segment(marker: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&marker.to_be_bytes());
    out.extend_from_slice(&((payload.len() as u16) + 2).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn build_siz(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    chroma_dx: u8,
    chroma_dy: u8,
    precision: u8,
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u16.to_be_bytes());
    payload.extend_from_slice(&width.to_be_bytes());
    payload.extend_from_slice(&height.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&tile_width.to_be_bytes());
    payload.extend_from_slice(&tile_height.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&3u16.to_be_bytes());
    payload.extend_from_slice(&[(precision - 1) & 0x7F, 1, 1]);
    payload.extend_from_slice(&[(precision - 1) & 0x7F, chroma_dx, chroma_dy]);
    payload.extend_from_slice(&[(precision - 1) & 0x7F, chroma_dx, chroma_dy]);
    segment(MARKER_SIZ, &payload)
}

fn build_siz_with_origins(
    image_origin_x: u32,
    image_origin_y: u32,
    tile_origin_x: u32,
    tile_origin_y: u32,
) -> Vec<u8> {
    let mut siz = build_siz(64, 64, 64, 64, 1, 1, 8);
    siz[14..18].copy_from_slice(&image_origin_x.to_be_bytes());
    siz[18..22].copy_from_slice(&image_origin_y.to_be_bytes());
    siz[30..34].copy_from_slice(&tile_origin_x.to_be_bytes());
    siz[34..38].copy_from_slice(&tile_origin_y.to_be_bytes());
    siz
}

fn build_cod(transform: u8, mct: bool) -> Vec<u8> {
    build_cod_with_code_block_style(transform, mct, 0)
}

fn build_cod_with_code_block_style(transform: u8, mct: bool, code_block_style: u8) -> Vec<u8> {
    let payload = [
        0,
        0,
        0,
        1,
        u8::from(mct),
        5,
        4,
        4,
        code_block_style,
        transform,
    ];
    segment(MARKER_COD, &payload)
}

fn build_qcd(style: u8) -> Vec<u8> {
    let mut payload = vec![0b0100_0000 | style];
    match style {
        1 => payload.extend_from_slice(&[0x08, 0x00]),
        2 => {
            for _ in 0..16 {
                payload.extend_from_slice(&[0x08, 0x00]);
            }
        }
        _ => payload.push(0x40),
    }
    segment(MARKER_QCD, &payload)
}

fn build_sot(tile_part_length: u32, tile_part_index: u8, tile_part_count: u8) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u16.to_be_bytes());
    payload.extend_from_slice(&tile_part_length.to_be_bytes());
    payload.push(tile_part_index);
    payload.push(tile_part_count);
    segment(MARKER_SOT, &payload)
}

fn build_supported_codestream(
    chroma_dx: u8,
    chroma_dy: u8,
    mct: bool,
    tile_width: u32,
    tile_height: u32,
    tile_part_count: u8,
) -> Vec<u8> {
    let mut stream = Vec::new();
    let entropy_data = [0x00, 0x01, 0x02, 0x03];
    let tile_part_length = 2 + (2 + 8) + 2 + entropy_data.len() as u32;
    stream.extend_from_slice(&MARKER_SOC.to_be_bytes());
    stream.extend_from_slice(&build_siz(
        512,
        256,
        tile_width,
        tile_height,
        chroma_dx,
        chroma_dy,
        8,
    ));
    stream.extend_from_slice(&build_cod(0, mct));
    stream.extend_from_slice(&build_qcd(2));
    stream.extend_from_slice(&build_sot(tile_part_length, 0, tile_part_count));
    stream.extend_from_slice(&MARKER_SOD.to_be_bytes());
    stream.extend_from_slice(&entropy_data);
    stream.extend_from_slice(&MARKER_EOC.to_be_bytes());
    stream
}

#[test]
fn parse_supported_codestream_header() {
    let stream = build_supported_codestream(2, 2, true, 512, 256, 1);
    let info = parse_codestream_header(&stream).unwrap();
    assert_eq!(info.image_width, 512);
    assert_eq!(info.image_height, 256);
    assert_eq!(info.tile_width, 512);
    assert_eq!(info.tile_height, 256);
    assert_eq!(info.tile_count_x, 1);
    assert_eq!(info.tile_count_y, 1);
    assert_eq!(info.components.len(), 3);
    assert_eq!(
        info.coding_style.transform,
        Jp2kWaveletTransform::Irreversible9x7
    );
    assert_eq!(
        info.quantization.style,
        Jp2kQuantizationStyle::ScalarExpounded
    );
    assert_eq!(info.quantization.steps.len(), 16);
    assert_eq!(info.quantization.steps[0].exponent, 1);
    assert_eq!(info.coding_style.code_block_width(), 64);
    assert_eq!(info.coding_style.code_block_height(), 64);
    assert_eq!(info.tile_parts.len(), 1);
    assert_eq!(info.tile_parts[0].header.tile_part_index, 0);
    assert_eq!(info.tile_parts[0].data_length, 4);
}

#[test]
fn narrow_subset_rejects_decoded_images_over_the_global_budget() {
    let stream = build_supported_codestream(2, 2, true, 512, 256, 1);
    let mut info = parse_codestream_header(&stream).unwrap();
    info.image_width = (512 * 1024 * 1024 / 3) + 1;
    info.image_height = 1;
    info.tile_width = info.image_width;
    info.tile_height = 1;

    assert!(matches!(
        validate_narrow_subset(&info),
        Err(WsiError::ResourceLimit { .. })
    ));
}

#[test]
fn narrow_subset_rejects_unsafe_coding_style_exponents() {
    let stream = build_supported_codestream(2, 2, true, 512, 256, 1);
    let mut info = parse_codestream_header(&stream).unwrap();
    info.coding_style.code_block_width_exponent = u8::MAX;
    assert!(validate_narrow_subset(&info).is_err());

    let mut info = parse_codestream_header(&stream).unwrap();
    info.coding_style.decomposition_levels = 31;
    info.quantization.steps = vec![
        Jp2kQuantStep {
            exponent: 1,
            mantissa: 0,
        };
        info.coding_style.expected_expounded_quant_steps()
    ];
    assert!(validate_narrow_subset(&info).is_err());
}

#[test]
fn validate_supported_subset_accepts_420() {
    let stream = build_supported_codestream(2, 2, true, 512, 256, 1);
    let info = parse_codestream_header(&stream).unwrap();
    validate_narrow_subset(&info).unwrap();
}

#[test]
fn validate_supported_subset_accepts_444() {
    let stream = build_supported_codestream(1, 1, false, 512, 256, 1);
    let info = parse_codestream_header(&stream).unwrap();
    validate_narrow_subset(&info).unwrap();
}

#[test]
fn reject_missing_soc() {
    let result = parse_codestream_header(&[0x00, 0x00, 0xFF, 0x51]);
    assert!(result.is_err());
}

#[test]
fn reject_siz_origins_beyond_image_and_tile_extents() {
    for (origins, expected) in [
        ((65, 0, 0, 0), "image width"),
        ((0, 65, 0, 0), "image height"),
        ((0, 0, 65, 0), "tile origin x"),
        ((0, 0, 0, 65), "tile origin y"),
    ] {
        let mut stream = MARKER_SOC.to_be_bytes().to_vec();
        stream.extend_from_slice(&build_siz_with_origins(
            origins.0, origins.1, origins.2, origins.3,
        ));

        let err = parse_codestream_header(&stream).unwrap_err().to_string();
        assert!(err.contains(expected), "{origins:?}: {err}");
    }
}

#[test]
fn reject_headers_missing_required_coding_and_quantization_markers() {
    let siz = build_siz(64, 64, 64, 64, 1, 1, 8);

    let mut missing_cod = MARKER_SOC.to_be_bytes().to_vec();
    missing_cod.extend_from_slice(&siz);
    missing_cod.extend_from_slice(&build_qcd(1));
    let err = parse_codestream_header(&missing_cod)
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing COD marker"), "{err}");

    let mut missing_qcd = MARKER_SOC.to_be_bytes().to_vec();
    missing_qcd.extend_from_slice(&siz);
    missing_qcd.extend_from_slice(&build_cod(1, false));
    let err = parse_codestream_header(&missing_qcd)
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing QCD marker"), "{err}");
}

#[test]
fn reject_non_8bit_subset() {
    let mut stream = Vec::new();
    stream.extend_from_slice(&MARKER_SOC.to_be_bytes());
    stream.extend_from_slice(&build_siz(64, 64, 64, 64, 1, 1, 12));
    stream.extend_from_slice(&build_cod(0, false));
    stream.extend_from_slice(&build_qcd(1));
    stream.extend_from_slice(&build_sot(14, 0, 1));
    stream.extend_from_slice(&MARKER_SOD.to_be_bytes());
    let info = parse_codestream_header(&stream).unwrap();
    let err = validate_narrow_subset(&info).unwrap_err().to_string();
    assert!(err.contains("unsigned 8-bit"));
}

#[test]
fn validate_supported_subset_accepts_reversible_lossless_transform() {
    let mut stream = Vec::new();
    stream.extend_from_slice(&MARKER_SOC.to_be_bytes());
    stream.extend_from_slice(&build_siz(64, 64, 64, 64, 1, 1, 8));
    stream.extend_from_slice(&build_cod(1, false));
    stream.extend_from_slice(&build_qcd(0));
    stream.extend_from_slice(&build_sot(14, 0, 1));
    stream.extend_from_slice(&MARKER_SOD.to_be_bytes());
    let info = parse_codestream_header(&stream).unwrap();
    validate_narrow_subset(&info).unwrap();
}

#[test]
fn validate_supported_subset_accepts_htj2k_lossless_profile() {
    let mut stream = Vec::new();
    stream.extend_from_slice(&MARKER_SOC.to_be_bytes());
    stream.extend_from_slice(&build_siz(64, 64, 64, 64, 1, 1, 8));
    stream.extend_from_slice(&build_cod_with_code_block_style(1, true, 0x40));
    stream.extend_from_slice(&build_qcd(0));
    stream.extend_from_slice(&build_sot(14, 0, 1));
    stream.extend_from_slice(&MARKER_SOD.to_be_bytes());
    let info = parse_codestream_header(&stream).unwrap();
    validate_narrow_subset(&info).unwrap();
}

#[test]
fn accept_decoder_supported_marker_segments() {
    let mut stream = build_supported_codestream(1, 1, false, 512, 256, 1);
    let insert_at = 2;
    stream.splice(insert_at..insert_at, segment(MARKER_POC, &[0, 0, 0]));
    let info = parse_codestream_header(&stream).unwrap();
    validate_narrow_subset(&info).unwrap();
}

#[test]
fn reject_invalid_expounded_quant_step_count() {
    let mut stream = Vec::new();
    stream.extend_from_slice(&MARKER_SOC.to_be_bytes());
    stream.extend_from_slice(&build_siz(64, 64, 64, 64, 1, 1, 8));
    stream.extend_from_slice(&build_cod(0, false));
    stream.extend_from_slice(&segment(MARKER_QCD, &[0b0100_0010, 0x08, 0x00]));
    stream.extend_from_slice(&build_sot(14, 0, 1));
    stream.extend_from_slice(&MARKER_SOD.to_be_bytes());
    let info = parse_codestream_header(&stream).unwrap();
    let err = validate_narrow_subset(&info).unwrap_err().to_string();
    assert!(err.contains("expounded quantization step count"));
}

#[test]
fn reject_multi_tile_subset() {
    let stream = build_supported_codestream(1, 1, false, 256, 256, 1);
    let info = parse_codestream_header(&stream).unwrap();
    let err = validate_narrow_subset(&info).unwrap_err().to_string();
    assert!(err.contains("single tile"));
}

#[test]
fn reject_multi_tile_part_subset() {
    let stream = build_supported_codestream(1, 1, false, 512, 256, 2);
    let info = parse_codestream_header(&stream).unwrap();
    let err = validate_narrow_subset(&info).unwrap_err().to_string();
    assert!(err.contains("tile-part count"));
}
