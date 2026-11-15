use super::super::dct_reemit::{encode_baseline_dct_image_with_markers, DctFrameMarkers};
use super::super::frame::{
    jpeg_header_segments, jpeg_segment_color_hint, merge_missing_jpeg_tables,
    parse_baseline_jpeg_frame_info, standalone_jpeg_frame_owned, JpegBitstreamColorHint,
};
use super::super::lossless::{
    lossless_crop_coefficient_bound, lossless_crop_coefficient_budget, lossless_crop_jpeg_stack,
    lossless_crop_or_whole_jpeg, lossless_crop_output_bound, lossless_crop_raw_tile, LosslessJpeg,
};
use super::*;
use crate::core::types::PixelRect;
use crate::test_support::jpeg_segments_through_first_scan;
use j2k_jpeg::transcode::{
    estimate_dct_windows, extract_dct_blocks, DctExtractOptions, JpegDctCodingMode,
    JpegDctComponent, JpegDctImage, McuRect,
};
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};

const BUDGET: u64 = 64 * 1024 * 1024;
const WIDTH: u32 = 100;
const HEIGHT: u32 = 76;
const ADOBE_RGB: [u8; 16] = [
    0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 0,
];

/// Detailed content, so most blocks carry nonzero AC coefficients.
fn texture(width: u32, height: u32, channels: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * channels) as usize);
    for y in 0..height {
        for x in 0..width {
            for c in 0..channels {
                let value = x * (7 + c * 3) + y * (5 + c * 11) + ((x ^ y) & 15) * 9 + c * 40;
                out.push((value & 0xFF) as u8);
            }
        }
    }
    out
}

fn encode_with(
    width: u32,
    height: u32,
    color: ColorType,
    sampling: SamplingFactor,
    configure: impl FnOnce(&mut Encoder<&mut Vec<u8>>),
) -> Vec<u8> {
    let channels = match color {
        ColorType::Luma => 1,
        ColorType::Rgb | ColorType::Bgr | ColorType::Ycbcr => 3,
        _ => 4,
    };
    let pixels = texture(width, height, channels);
    let mut encoded = Vec::new();
    let mut encoder = Encoder::new(&mut encoded, 85);
    encoder.set_sampling_factor(sampling);
    configure(&mut encoder);
    encoder
        .encode(&pixels, width as u16, height as u16, color)
        .unwrap();
    encoded
}

fn encode(color: ColorType, sampling: SamplingFactor) -> Vec<u8> {
    encode_with(WIDTH, HEIGHT, color, sampling, |_| {})
}

fn dct(data: &[u8]) -> JpegDctImage {
    extract_dct_blocks(data, DctExtractOptions::default()).unwrap()
}

fn mcu_size(image: &JpegDctImage) -> (u32, u32) {
    let max_h = image.components.iter().map(|c| c.h_samp).max().unwrap();
    let max_v = image.components.iter().map(|c| c.v_samp).max().unwrap();
    (u32::from(max_h) * 8, u32::from(max_v) * 8)
}

struct ExpectedCrop {
    width: u32,
    height: u32,
    region: (u32, u32, u32, u32),
    mcu_col: usize,
    mcu_row: usize,
}

fn expected_crop(
    (x, y, w, h): (u32, u32, u32, u32),
    (src_width, src_height): (u32, u32),
    (mcu_width, mcu_height): (u32, u32),
) -> ExpectedCrop {
    let (x0, y0) = (x / mcu_width * mcu_width, y / mcu_height * mcu_height);
    let (x1, y1) = ((x + w).min(src_width), (y + h).min(src_height));
    ExpectedCrop {
        width: x1 - x0,
        height: y1 - y0,
        region: (x - x0, y - y0, x1 - x, y1 - y),
        mcu_col: (x0 / mcu_width) as usize,
        mcu_row: (y0 / mcu_height) as usize,
    }
}

/// Every output block equals the source block at the window offset.
fn assert_block_window(
    source: &JpegDctImage,
    output: &JpegDctImage,
    mcu_col: usize,
    mcu_row: usize,
) {
    assert_eq!(output.components.len(), source.components.len());
    for (idx, (src, out)) in source.components.iter().zip(&output.components).enumerate() {
        assert_eq!((out.h_samp, out.v_samp), (src.h_samp, src.v_samp));
        assert_eq!(
            out.quant_table, src.quant_table,
            "component {idx} quant table"
        );
        let (h, v) = (usize::from(src.h_samp), usize::from(src.v_samp));
        let (out_cols, src_cols) = (out.block_cols as usize, src.block_cols as usize);
        for by in 0..out.block_rows as usize {
            for bx in 0..out_cols {
                let src_idx = (by + mcu_row * v) * src_cols + bx + mcu_col * h;
                assert_eq!(
                    out.quantized_blocks[by * out_cols + bx],
                    src.quantized_blocks[src_idx],
                    "component {idx} block ({bx}, {by})"
                );
            }
        }
    }
}

fn assert_crop_matches(source: &[u8], rect: (u32, u32, u32, u32), label: &str) -> LosslessJpeg {
    let src = dct(source);
    let crop = crop_or_whole(source.to_vec(), rect, BUDGET)
        .unwrap_or_else(|err| panic!("{label} {rect:?}: {err}"));
    let expected = expected_crop(rect, (src.width, src.height), mcu_size(&src));
    assert_eq!(
        (crop.width, crop.height),
        (expected.width, expected.height),
        "{label} {rect:?}"
    );
    assert_eq!(crop.tile_region, expected.region, "{label} {rect:?}");
    assert_eq!(
        (crop.info.width, crop.info.height),
        (crop.width, crop.height)
    );
    assert_eq!(crop.info.bits_allocated, 8);
    assert_eq!(
        usize::from(crop.info.samples_per_pixel),
        src.components.len()
    );

    let sof = parse_baseline_jpeg_frame_info(&crop.data).expect("output is baseline SOF0");
    assert_eq!((sof.width, sof.height), (crop.width, crop.height));
    let out = dct(&crop.data);
    assert_eq!(out.coding_mode, JpegDctCodingMode::BaselineSequential);
    assert_eq!(out.scan_count, 1);
    assert!(
        out.restart_index.is_none(),
        "{label}: output keeps no restart interval"
    );
    assert_eq!(out.color_space, src.color_space, "{label} {rect:?}");
    assert_block_window(&src, &out, expected.mcu_col, expected.mcu_row);
    crop
}

fn decode_rgb(data: &[u8]) -> (u32, Vec<u8>) {
    let tile = decode_standalone_jpeg(data).unwrap();
    (tile.width, tile.as_u8().unwrap().to_vec())
}

fn crop_rgb(pixels: &[u8], width: u32, (x, y, w, h): (u32, u32, u32, u32)) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 3) as usize);
    for row in y..y + h {
        let start = ((row * width + x) * 3) as usize;
        out.extend_from_slice(&pixels[start..start + (w * 3) as usize]);
    }
    out
}

/// The crop's `tile_region` decodes to the source's pixels at `rect`, with
/// both this crate's decoder and an independent one.
fn assert_crop_pixels(source: &[u8], crop: &LosslessJpeg, rect: (u32, u32, u32, u32)) {
    let (src_width, src_pixels) = decode_rgb(source);
    let (crop_width, crop_pixels) = decode_rgb(&crop.data);
    let (x, y, w, h) = crop.tile_region;
    let requested = (rect.0, rect.1, w, h);
    assert_eq!(
        crop_rgb(&crop_pixels, crop_width, (x, y, w, h)),
        crop_rgb(&src_pixels, src_width, requested),
        "{rect:?}"
    );

    let independent = |data: &[u8]| {
        let mut decoder = jpeg_decoder::Decoder::new(data);
        let pixels = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        (u32::from(info.width), pixels, info.pixel_format)
    };
    let (src_width, src_pixels, format) = independent(source);
    let (crop_width, crop_pixels, crop_format) = independent(&crop.data);
    assert_eq!(format, crop_format);
    let channels = match format {
        jpeg_decoder::PixelFormat::L8 => 1,
        jpeg_decoder::PixelFormat::RGB24 => 3,
        other => panic!("unexpected jpeg-decoder format {other:?}"),
    };
    let window = |pixels: &[u8], width: u32, (x, y, w, h): (u32, u32, u32, u32)| {
        (y..y + h)
            .flat_map(|row| {
                let start = ((row * width + x) * channels) as usize;
                pixels[start..start + (w * channels) as usize].to_vec()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        window(&crop_pixels, crop_width, (x, y, w, h)),
        window(&src_pixels, src_width, requested),
        "independent decoder {rect:?}"
    );
}

const RECTS: [(u32, u32, u32, u32); 7] = [
    (0, 0, 32, 32),
    (16, 16, 48, 32),
    (5, 9, 40, 30),
    (37, 21, 17, 3),
    (64, 48, 64, 64),
    (99, 75, 1, 1),
    (0, 0, WIDTH, HEIGHT),
];

#[test]
fn crops_keep_quantized_coefficients_for_every_sampling_and_origin() {
    for (label, color, sampling) in [
        ("4:4:4", ColorType::Rgb, SamplingFactor::F_1_1),
        ("4:2:2", ColorType::Rgb, SamplingFactor::F_2_1),
        ("4:2:0", ColorType::Rgb, SamplingFactor::F_2_2),
        ("4:4:0", ColorType::Rgb, SamplingFactor::F_1_2),
        ("gray", ColorType::Luma, SamplingFactor::F_1_1),
    ] {
        let source = encode(color, sampling);
        for rect in RECTS {
            assert_crop_matches(&source, rect, label);
        }
    }
}

#[test]
fn crop_origin_snaps_down_to_the_mcu_grid_and_far_edges_clamp() {
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);

    let aligned = crop_or_whole(source.clone(), (16, 32, 32, 16), BUDGET).unwrap();
    assert_eq!((aligned.width, aligned.height), (32, 16));
    assert_eq!(aligned.tile_region, (0, 0, 32, 16));

    let unaligned = crop_or_whole(source.clone(), (21, 40, 30, 20), BUDGET).unwrap();
    assert_eq!((unaligned.width, unaligned.height), (35, 28));
    assert_eq!(unaligned.tile_region, (5, 8, 30, 20));

    let edge = crop_or_whole(source.clone(), (64, 48, 64, 64), BUDGET).unwrap();
    assert_eq!((edge.width, edge.height), (36, 28));
    assert_eq!(edge.tile_region, (0, 0, 36, 28));
}

#[test]
fn unsubsampled_crops_decode_to_the_source_pixels() {
    for (color, sampling) in [
        (ColorType::Rgb, SamplingFactor::F_1_1),
        (ColorType::Luma, SamplingFactor::F_1_1),
    ] {
        let source = encode(color, sampling);
        for rect in RECTS {
            let crop = assert_crop_matches(&source, rect, "pixels");
            assert_crop_pixels(&source, &crop, rect);
        }
    }
}

#[test]
fn progressive_and_multi_scan_sources_reemit_as_baseline() {
    let progressive = encode_with(WIDTH, HEIGHT, ColorType::Rgb, SamplingFactor::F_2_2, |e| {
        e.set_progressive(true)
    });
    assert_eq!(
        dct(&progressive).coding_mode,
        JpegDctCodingMode::Progressive
    );
    let multi_scan = encode_with(WIDTH, HEIGHT, ColorType::Rgb, SamplingFactor::F_2_1, |e| {
        e.set_optimized_huffman_tables(true)
    });
    let multi_scan_source = dct(&multi_scan);
    assert_eq!(
        multi_scan_source.coding_mode,
        JpegDctCodingMode::BaselineSequential
    );
    assert!(
        multi_scan_source.scan_count > 1,
        "fixture must split components over scans"
    );
    let gray_progressive =
        encode_with(WIDTH, HEIGHT, ColorType::Luma, SamplingFactor::F_1_1, |e| {
            e.set_progressive(true)
        });

    for (label, source) in [
        ("progressive", &progressive),
        ("multi-scan", &multi_scan),
        ("gray progressive", &gray_progressive),
    ] {
        for rect in RECTS {
            assert_crop_matches(source, rect, label);
        }
    }
    let crop = assert_crop_matches(&gray_progressive, (5, 9, 40, 30), "gray progressive");
    assert_crop_pixels(&gray_progressive, &crop, (5, 9, 40, 30));
}

#[test]
fn restart_intervals_are_dropped() {
    let source = encode_with(WIDTH, HEIGHT, ColorType::Rgb, SamplingFactor::F_2_2, |e| {
        e.set_restart_interval(2)
    });
    assert!(dct(&source).restart_index.is_some());

    let crop = assert_crop_matches(&source, (16, 16, 48, 32), "restart");

    assert!(
        jpeg_header_segments(&crop.data).all(|segment| segment.marker != 0xDD),
        "output must not carry DRI"
    );
}

/// Replace jpeg-encoder's 0-based component IDs in SOF and the first SOS.
fn with_component_ids(jpeg: &[u8], ids: [u8; 3]) -> Vec<u8> {
    let mut out = jpeg.to_vec();
    for segment in jpeg_segments_through_first_scan(jpeg) {
        let payload = segment.start + 4;
        match segment.marker {
            0xC0 => {
                for (idx, id) in ids.iter().enumerate() {
                    out[payload + 6 + idx * 3] = *id;
                }
            }
            0xDA => {
                for idx in 0..usize::from(segment.payload[0]) {
                    let selector = &mut out[payload + 1 + idx * 2];
                    *selector = ids[usize::from(*selector)];
                }
            }
            _ => {}
        }
    }
    out
}

fn sof_component_ids(jpeg: &[u8]) -> Vec<u8> {
    let sof = jpeg_segments_through_first_scan(jpeg)
        .into_iter()
        .find(|segment| segment.marker == 0xC0)
        .expect("no SOF0");
    let count = usize::from(sof.payload[5]);
    (0..count).map(|idx| sof.payload[6 + idx * 3]).collect()
}

#[test]
fn rgb_coded_sources_stay_rgb_coded() {
    // Raw component samples, interpreted as untransformed RGB by the markers.
    let samples = encode(ColorType::Ycbcr, SamplingFactor::F_1_1);
    let mut adobe = crate::test_support::jpeg_without_jfif(&samples);
    adobe.splice(2..2, ADOBE_RGB);
    let rgb_ids = with_component_ids(&crate::test_support::jpeg_without_jfif(&samples), *b"RGB");

    for (label, source, ids) in [
        ("adobe", &adobe, vec![0, 1, 2]),
        ("component ids", &rgb_ids, b"RGB".to_vec()),
    ] {
        assert_eq!(
            dct(source).color_space,
            j2k_jpeg::ColorSpace::Rgb,
            "{label} fixture"
        );
        for rect in [(5, 9, 40, 30), (64, 48, 64, 64)] {
            let crop = assert_crop_matches(source, rect, label);
            assert_eq!(
                crop.info.photometric_interpretation,
                EncodedTilePhotometricInterpretation::Rgb,
                "{label}"
            );
            assert_eq!(
                jpeg_segment_color_hint(&crop.data),
                JpegBitstreamColorHint::Rgb
            );
            assert_eq!(sof_component_ids(&crop.data), ids, "{label}");
            let (src_width, src_pixels) = decode_rgb(source);
            let (crop_width, crop_pixels) = decode_rgb(&crop.data);
            let (x, y, w, h) = crop.tile_region;
            assert_eq!(
                crop_rgb(&crop_pixels, crop_width, (x, y, w, h)),
                crop_rgb(&src_pixels, src_width, (rect.0, rect.1, w, h)),
                "{label} {rect:?}"
            );
        }
    }
    let crop = crop_or_whole(adobe.clone(), (5, 9, 40, 30), BUDGET).unwrap();
    assert!(
        crop.data
            .windows(ADOBE_RGB.len())
            .any(|window| window == ADOBE_RGB),
        "the source Adobe APP14 segment is carried over"
    );
    assert_crop_pixels(&adobe, &crop, (5, 9, 40, 30));
}

#[test]
fn jfif_ycbcr_sources_stay_ycbcr() {
    let source = encode(ColorType::Rgb, SamplingFactor::F_1_1);
    let crop = assert_crop_matches(&source, (5, 9, 40, 30), "jfif");
    assert_eq!(
        crop.info.photometric_interpretation,
        EncodedTilePhotometricInterpretation::YbrFull422
    );
    assert_eq!(&crop.data[..4], &[0xFF, 0xD8, 0xFF, 0xE0]);
    assert_eq!(&crop.data[6..11], b"JFIF\0");
    assert_eq!(sof_component_ids(&crop.data), sof_component_ids(&source));

    let gray = crop_or_whole(
        encode(ColorType::Luma, SamplingFactor::F_1_1),
        (0, 0, 8, 8),
        BUDGET,
    )
    .unwrap();
    assert_eq!(
        gray.info.photometric_interpretation,
        EncodedTilePhotometricInterpretation::Monochrome2
    );
}

#[test]
fn distinct_cb_and_cr_quant_tables_survive() {
    let mut image = dct(&encode(ColorType::Rgb, SamplingFactor::F_2_1));
    let mut cr_table = image.components[1].quant_table;
    for value in &mut cr_table {
        *value = (*value + 3).min(255);
    }
    image.components[2].quant_table = cr_table;
    let source = encode_baseline_dct_image_with_markers(
        &image,
        &DctFrameMarkers {
            app_segments: Vec::new(),
            component_ids: vec![1, 2, 3],
        },
    )
    .unwrap();
    let source_dct = dct(&source);
    assert_ne!(
        source_dct.components[1].quant_table,
        source_dct.components[2].quant_table
    );
    assert_eq!(source_dct.components[2].quant_table, cr_table);

    for rect in RECTS {
        let crop = assert_crop_matches(&source, rect, "distinct chroma tables");
        let out = dct(&crop.data);
        assert_eq!(out.components[2].quant_table, cr_table);
    }
}

/// One image stored as standalone JPEGs of `heights` rows, top to bottom.
fn stacked_parts(width: u32, heights: &[u32], sampling: SamplingFactor) -> (Vec<Vec<u8>>, Vec<u8>) {
    let total: u32 = heights.iter().sum();
    let pixels = texture(width, total, 3);
    let mut parts = Vec::new();
    let mut row = 0;
    for &height in heights {
        let start = (row * width * 3) as usize;
        let end = ((row + height) * width * 3) as usize;
        let mut encoded = Vec::new();
        let mut encoder = Encoder::new(&mut encoded, 85);
        encoder.set_sampling_factor(sampling);
        encoder
            .encode(
                &pixels[start..end],
                width as u16,
                height as u16,
                ColorType::Rgb,
            )
            .unwrap();
        parts.push(encoded);
        row += height;
    }
    (parts, pixels)
}

/// The stack's coefficient planes as one image: every part but the last is
/// a whole number of MCU rows, so the block grids concatenate.
fn stacked_dct(parts: &[Vec<u8>]) -> JpegDctImage {
    let images = parts.iter().map(|part| dct(part)).collect::<Vec<_>>();
    let first = &images[0];
    let components = (0..first.components.len())
        .map(|idx| {
            let reference = &first.components[idx];
            JpegDctComponent {
                component_index: idx,
                width: reference.width,
                height: images
                    .iter()
                    .map(|image| image.components[idx].height)
                    .sum(),
                h_samp: reference.h_samp,
                v_samp: reference.v_samp,
                block_cols: reference.block_cols,
                block_rows: images
                    .iter()
                    .map(|image| image.components[idx].block_rows)
                    .sum(),
                quant_table: reference.quant_table,
                quantized_blocks: images
                    .iter()
                    .flat_map(|image| image.components[idx].quantized_blocks.clone())
                    .collect(),
                dequantized_blocks: Vec::new(),
            }
        })
        .collect();
    JpegDctImage {
        width: first.width,
        height: images.iter().map(|image| image.height).sum(),
        color_space: first.color_space,
        coding_mode: JpegDctCodingMode::BaselineSequential,
        scan_count: 1,
        components,
        restart_index: None,
    }
}

#[test]
fn stacked_parts_crop_across_part_boundaries() {
    for (sampling, heights) in [
        (SamplingFactor::F_2_2, [32, 32, 20]),
        (SamplingFactor::F_1_1, [24, 8, 13]),
    ] {
        let (parts, _) = stacked_parts(64, &heights, sampling);
        let refs = parts.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let stacked = stacked_dct(&parts);
        let total = heights.iter().sum::<u32>();
        for rect in [
            (8, 24, 40, 20),
            (0, 28, 64, 20),
            (3, total - 10, 30, 40),
            (0, 0, 64, total),
            (10, 33, 5, 5),
        ] {
            let crop = lossless_crop_jpeg_stack(&refs, rect, BUDGET)
                .unwrap_or_else(|err| panic!("{rect:?}: {err}"));
            let expected = expected_crop(rect, (64, total), mcu_size(&stacked));
            assert_eq!((crop.width, crop.height), (expected.width, expected.height));
            assert_eq!(crop.tile_region, expected.region);
            let out = dct(&crop.data);
            assert_eq!((out.width, out.height), (crop.width, crop.height));
            assert_block_window(&stacked, &out, expected.mcu_col, expected.mcu_row);
        }
    }
}

#[test]
fn stacked_4_4_4_crop_spanning_parts_decodes_to_the_source_pixels() {
    let heights = [24, 8, 13];
    let (parts, _) = stacked_parts(64, &heights, SamplingFactor::F_1_1);
    let refs = parts.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let rect = (3, 20, 30, 18);
    let crop = lossless_crop_jpeg_stack(&refs, rect, BUDGET).unwrap();

    let mut stacked_pixels = Vec::new();
    for part in &parts {
        stacked_pixels.extend(decode_rgb(part).1);
    }
    let (crop_width, crop_pixels) = decode_rgb(&crop.data);
    let (x, y, w, h) = crop.tile_region;
    assert_eq!(
        crop_rgb(&crop_pixels, crop_width, (x, y, w, h)),
        crop_rgb(&stacked_pixels, 64, (rect.0, rect.1, w, h))
    );
}

/// Keep the header and the first entropy bytes, then end the image.
fn with_truncated_scan(jpeg: &[u8]) -> Vec<u8> {
    let mut out = jpeg[..first_scan_start(jpeg) + 4].to_vec();
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// Offset of the first entropy-coded byte: the end of the first SOS header.
fn first_scan_start(jpeg: &[u8]) -> usize {
    jpeg_segments_through_first_scan(jpeg)
        .pop()
        .filter(|segment| segment.marker == 0xDA)
        .expect("no SOS")
        .end
}

#[test]
fn stacked_crop_entropy_decodes_only_touched_parts() {
    let (mut parts, _) = stacked_parts(64, &[32, 32, 16], SamplingFactor::F_2_2);
    parts[2] = with_truncated_scan(&parts[2]);
    assert!(extract_dct_blocks(&parts[2], DctExtractOptions::default()).is_err());
    let refs = parts.iter().map(Vec::as_slice).collect::<Vec<_>>();

    lossless_crop_jpeg_stack(&refs, (0, 10, 64, 40), BUDGET)
        .expect("parts 0 and 1 suffice for this crop");
    let err = lossless_crop_jpeg_stack(&refs, (0, 60, 64, 10), BUDGET).unwrap_err();
    assert!(
        matches!(&err, WsiError::Jpeg(reason) if reason.contains("stack part 2")),
        "{err:?}"
    );
}

fn unsupported_reason(result: Result<LosslessJpeg, WsiError>) -> String {
    match result {
        Err(WsiError::Unsupported { reason }) => reason,
        other => panic!(
            "expected Unsupported, got {:?}",
            other.map(|crop| crop.tile_region)
        ),
    }
}

#[test]
fn stacked_parts_must_share_their_coding_profile() {
    let (parts, _) = stacked_parts(64, &[32, 32], SamplingFactor::F_2_2);
    let narrow = stacked_parts(48, &[32], SamplingFactor::F_2_2).0.remove(0);
    let unsubsampled = stacked_parts(64, &[32], SamplingFactor::F_1_1).0.remove(0);
    let mut lower_quality = Vec::new();
    let mut encoder = Encoder::new(&mut lower_quality, 40);
    encoder.set_sampling_factor(SamplingFactor::F_2_2);
    encoder
        .encode(&texture(64, 32, 3), 64, 32, ColorType::Rgb)
        .unwrap();
    let mut adobe = crate::test_support::jpeg_without_jfif(&parts[1]);
    adobe.splice(2..2, ADOBE_RGB);

    // Frame headers are matched for every part; quant tables, which j2k-jpeg
    // reports with extracted coefficients, for the parts a crop touches.
    for (second, rect, expected) in [
        (
            &narrow,
            (0, 0, 8, 8),
            "part 1 is 48 pixels wide, part 0 is 64",
        ),
        (
            &unsubsampled,
            (0, 0, 8, 8),
            "part 1 component 0 samples 1x1, part 0 samples 2x2",
        ),
        (
            &lower_quality,
            (0, 24, 8, 16),
            "part 1 component 0 uses a different quant table",
        ),
        (
            &adobe,
            (0, 0, 8, 8),
            "part 1 signals color with different JFIF/Adobe markers",
        ),
    ] {
        let reason =
            unsupported_reason(lossless_crop_jpeg_stack(&[&parts[0], second], rect, BUDGET));
        assert!(reason.contains(expected), "{reason}");
    }

    let (short, _) = stacked_parts(64, &[24, 32], SamplingFactor::F_2_2);
    let refs = short.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let reason = unsupported_reason(lossless_crop_jpeg_stack(&refs, (0, 0, 8, 8), BUDGET));
    assert!(
        reason.contains("part 0 is 24 rows tall, not a multiple of the 16-row MCU height"),
        "{reason}"
    );
    let reason = unsupported_reason(lossless_crop_jpeg_stack(&[], (0, 0, 8, 8), BUDGET));
    assert!(reason.contains("at least one JPEG"), "{reason}");
}

#[test]
fn stacked_part_headers_are_checked_before_any_part_is_decoded() {
    let (parts, _) = stacked_parts(64, &[32, 32], SamplingFactor::F_2_2);
    let crop = |second: &[u8]| lossless_crop_jpeg_stack(&[&parts[0], second], (0, 0, 8, 8), BUDGET);
    let sof = jpeg_segments_through_first_scan(&parts[1])
        .into_iter()
        .find(|segment| segment.marker == 0xC0)
        .unwrap();

    // The crop touches only part 0, yet every part's header is read first.
    // A part j2k-jpeg cannot walk, or without a frame, is a JPEG error
    // naming the part.
    for (second, expected) in [
        (
            [&parts[1][..sof.start], &parts[1][sof.end..]].concat(),
            "no frame before its first scan",
        ),
        (parts[1][..sof.end].to_vec(), "could not read its source"),
        (parts[1][2..].to_vec(), "could not read its source"),
    ] {
        match crop(&second) {
            Err(WsiError::Jpeg(reason)) => {
                assert!(reason.starts_with("stack part 1: "), "{reason}");
                assert!(reason.contains(expected), "{reason}");
            }
            other => panic!(
                "expected a JPEG error for {expected}, got {:?}",
                other.map(|jpeg| jpeg.tile_region)
            ),
        }
    }

    // Valid parts the crop does not cut, or that do not match part 0, are
    // unsupported.
    let gray = encode_with(64, 32, ColorType::Luma, SamplingFactor::F_1_1, |_| {});
    for (second, expected) in [
        (
            patched_sof(&parts[1], Some(0xC1), None),
            "stack part 1: lossless JPEG crop cuts 8-bit baseline and progressive JPEG of 1 or 3 components, got Extended8 with 3 components",
        ),
        (
            patched_sof(&parts[1], None, Some((0, 12))),
            "stack part 1: lossless JPEG crop does not cut this JPEG: unsupported bit depth: 12",
        ),
        (gray, "part 1 has components [0], part 0 has [0, 1, 2]"),
        (
            with_component_ids(&parts[1], [1, 2, 3]),
            "part 1 has components [1, 2, 3], part 0 has [0, 1, 2]",
        ),
    ] {
        let reason = unsupported_reason(crop(&second));
        assert!(reason.contains(expected), "{reason}");
    }
}

#[test]
fn codec_errors_are_sorted_into_unavailable_crops_and_invalid_input() {
    use super::super::lossless::CropFailure;
    use j2k_jpeg::transcode::{JpegDctImageError, LosslessCropError};
    use j2k_jpeg::{HuffmanFailure, JpegEncodeError, JpegError, SofKind};

    let unavailable = |err: LosslessCropError| match CropFailure::from(err) {
        CropFailure::Unavailable(err) => err,
        CropFailure::Invalid(err) => panic!("expected an unavailable crop, got {err:?}"),
    };
    let invalid = |err: LosslessCropError| match CropFailure::from(err) {
        CropFailure::Invalid(err) => err,
        CropFailure::Unavailable(err) => panic!("expected an invalid crop, got {err:?}"),
    };

    // Working storage beyond the budget keeps its typed byte counts.
    assert!(matches!(
        unavailable(
            JpegError::MemoryCapExceeded {
                requested: 4096,
                cap: 1024
            }
            .into()
        ),
        WsiError::ResourceLimit {
            resource: "lossless JPEG crop DCT coefficients",
            requested: 4096,
            limit: 1024
        }
    ));
    assert!(matches!(
        unavailable(
            JpegEncodeError::MemoryCapExceeded {
                requested: 9,
                cap: 8
            }
            .into()
        ),
        WsiError::ResourceLimit {
            resource: "lossless JPEG crop output",
            requested: 9,
            limit: 8
        }
    ));
    // Valid JPEG the codec does not cut or cannot re-emit as baseline.
    for err in [
        LosslessCropError::from(JpegError::NotImplemented {
            sof: SofKind::Progressive12,
        }),
        JpegError::UnsupportedComponentCount { count: 2 }.into(),
        JpegError::ZeroDimension {
            width: 0,
            height: 8,
        }
        .into(),
        JpegError::DimensionOverflow {
            width: 65_535,
            height: 8,
        }
        .into(),
        JpegEncodeError::InvalidDctImage {
            reason: JpegDctImageError::UnsupportedGrayscaleSampling {
                h_samp: 2,
                v_samp: 2,
            },
        }
        .into(),
    ] {
        let text = err.to_string();
        match unavailable(err) {
            WsiError::Unsupported { reason } => assert!(
                reason.ends_with(text.trim_start_matches("invalid JPEG DCT image: ")),
                "{reason}"
            ),
            other => panic!("expected Unsupported for {text}, got {other:?}"),
        }
    }
    // Corrupt streams and codec failures are never an unavailable crop.
    for err in [
        LosslessCropError::from(JpegError::Truncated {
            offset: 10,
            expected: 2,
        }),
        JpegError::InvalidMarker {
            offset: 2,
            marker: 0x0D,
        }
        .into(),
        JpegError::HuffmanDecode {
            mcu: 3,
            reason: HuffmanFailure::InvalidSymbol,
        }
        .into(),
        JpegError::MissingQuantTable {
            component: 0,
            table_id: 0,
        }
        .into(),
        JpegError::HostAllocationFailed { bytes: 7 }.into(),
        JpegError::InternalInvariant { reason: "test" }.into(),
        JpegEncodeError::EmptyDimensions.into(),
        JpegEncodeError::HostAllocationFailed { bytes: 7 }.into(),
    ] {
        let text = err.to_string();
        match invalid(err) {
            WsiError::Jpeg(reason) => assert!(reason.ends_with(&text), "{reason}"),
            other => panic!("expected a JPEG error for {text}, got {other:?}"),
        }
    }
}

#[test]
fn crop_rejects_rectangles_outside_the_source_or_empty() {
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);
    for (rect, expected) in [
        ((0, 0, 0, 8), "nonzero rectangle"),
        ((0, 0, 8, 0), "nonzero rectangle"),
        ((WIDTH, 0, 1, 1), "lies outside the 100x76 image"),
        ((0, HEIGHT, 1, 1), "lies outside the 100x76 image"),
        ((10, 0, u32::MAX, 1), "overflows u32"),
    ] {
        let reason = unsupported_reason(crop_or_whole(source.clone(), rect, BUDGET));
        assert!(reason.contains(expected), "{rect:?}: {reason}");
    }
}

/// Asserts `result` is `source` returned whole, with `rect`, clamped to the
/// image as a crop clamps it, located inside.
/// [`lossless_crop_or_whole_jpeg`] with no output limit.
fn crop_or_whole(
    source: Vec<u8>,
    rect: (u32, u32, u32, u32),
    max_coefficient_bytes: u64,
) -> Result<LosslessJpeg, WsiError> {
    lossless_crop_or_whole_jpeg(source, rect, max_coefficient_bytes, u64::MAX)
}

#[test]
fn crop_larger_than_the_output_limit_returns_the_fitting_source() {
    // Optimized Huffman tables code the source smaller than the standard
    // tables a crop is written with.
    let source = encode_with(
        WIDTH,
        HEIGHT,
        ColorType::Rgb,
        SamplingFactor::F_1_1,
        |encoder| encoder.set_optimized_huffman_tables(true),
    );
    let rect = (0, 0, WIDTH, HEIGHT);
    let crop = crop_or_whole(source.clone(), rect, BUDGET).unwrap();
    assert!(
        crop.data.len() > source.len(),
        "{} > {}",
        crop.data.len(),
        source.len()
    );

    let limit = source.len() as u64;
    let whole = lossless_crop_or_whole_jpeg(source.clone(), rect, BUDGET, limit).unwrap();
    assert_whole_jpeg(&whole, &source, rect);
    // A source over the limit too keeps the crop; the caller's limit check
    // then refuses whichever it gets.
    let crop = lossless_crop_or_whole_jpeg(source.clone(), rect, BUDGET, limit - 1).unwrap();
    assert_ne!(crop.data, source);
}

fn assert_whole_jpeg(result: &LosslessJpeg, source: &[u8], rect: (u32, u32, u32, u32)) {
    let src = dct(source);
    let (x, y, w, h) = rect;
    assert_eq!(result.data, source, "the stored JPEG comes back unchanged");
    assert_eq!((result.width, result.height), (src.width, src.height));
    assert_eq!(
        result.tile_region,
        (
            x,
            y,
            (x + w).min(src.width) - x,
            (y + h).min(src.height) - y
        )
    );
    assert_eq!(
        (result.info.width, result.info.height),
        (src.width, src.height)
    );
    assert_eq!(result.info.bits_allocated, 8);
    assert_eq!(
        usize::from(result.info.samples_per_pixel),
        src.components.len()
    );
}

/// The MCU window a crop of `rect` extracts from `source`, from its headers.
fn crop_mcu_window(source: &[u8], (x, y, w, h): (u32, u32, u32, u32)) -> McuRect {
    let info = j2k_jpeg::Decoder::inspect(source).unwrap();
    let mcu = info.mcu_geometry;
    let (x_end, y_end) = (
        (x + w).min(info.dimensions.0),
        (y + h).min(info.dimensions.1),
    );
    McuRect {
        x: x / mcu.width,
        y: y / mcu.height,
        width: x_end.div_ceil(mcu.width) - x / mcu.width,
        height: y_end.div_ceil(mcu.height) - y / mcu.height,
    }
}

/// The working set j2k-jpeg estimates, from `source`'s headers alone, for
/// extracting the quantized blocks of `window`. It gates every crop.
fn extraction_estimate(source: &[u8], window: McuRect) -> u64 {
    estimate_dct_windows(source, &[window], DctExtractOptions::quantized_only())
        .unwrap()
        .peak_bytes as u64
}

fn crop_estimate(source: &[u8], rect: (u32, u32, u32, u32)) -> u64 {
    extraction_estimate(source, crop_mcu_window(source, rect))
}

#[test]
fn crop_over_its_coefficient_budget_returns_the_whole_jpeg_before_decoding() {
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);
    let rect = (16, 16, 32, 32);
    // 7x5 MCUs of 6 blocks, 128 bytes a quantized block. A single-scan
    // baseline source holds only the 2x2-MCU window's blocks.
    let whole = 7 * 5 * 6 * 128;
    let window = 2 * 2 * 6 * 128;
    let needed = crop_estimate(&source, rect);
    assert!((window..whole).contains(&needed), "{needed}");
    let fallback = crop_or_whole(source.clone(), rect, needed - 1).unwrap();
    assert_whole_jpeg(&fallback, &source, rect);
    let crop = crop_or_whole(source.clone(), rect, needed).expect("exact budget suffices");
    assert_eq!((crop.width, crop.height), (32, 32));

    let progressive = encode_with(WIDTH, HEIGHT, ColorType::Rgb, SamplingFactor::F_2_2, |e| {
        e.set_progressive(true)
    });
    // Progressive extraction accumulates the whole image: a 32-bit plane
    // beside the quantized one.
    let needed = crop_estimate(&progressive, rect);
    assert!(needed >= 3 * whole, "{needed}");
    let fallback = crop_or_whole(progressive.clone(), rect, needed - 1).unwrap();
    assert_whole_jpeg(&fallback, &progressive, rect);
    let crop = crop_or_whole(progressive, rect, needed).unwrap();
    assert_eq!((crop.width, crop.height), (32, 32));

    // A truncated scan is never entropy-decoded when the budget is short:
    // the headers alone return the stored JPEG. With the budget, the same
    // scan is decoded and is an error.
    let truncated = with_truncated_scan(&source);
    let needed = crop_estimate(&truncated, rect);
    for short in [0, needed - 1] {
        let fallback = crop_or_whole(truncated.clone(), rect, short).unwrap();
        assert_eq!(fallback.data, truncated);
    }
    assert!(matches!(
        crop_or_whole(truncated, rect, needed),
        Err(WsiError::Jpeg(_))
    ));
}

#[test]
fn valid_jpegs_the_crop_cannot_handle_return_whole() {
    // A lone component's MCU is one block whatever its sampling factors, so
    // 2x2 codes the same image as 1x1; the crop accepts only 1x1.
    let mut source = encode(ColorType::Luma, SamplingFactor::F_1_1);
    let sof = source
        .windows(2)
        .position(|marker| marker == [0xFF, 0xC0])
        .unwrap();
    assert_eq!(source[sof + 11], 0x11, "Luma SOF samples 1x1");
    source[sof + 11] = 0x22;
    let rect = (21, 40, 30, 20);

    let whole = crop_or_whole(source.clone(), rect, BUDGET).unwrap();
    assert_eq!(whole.data, source);
    assert_eq!((whole.width, whole.height), (WIDTH, HEIGHT));
    assert_eq!(whole.tile_region, rect);
    assert_eq!(
        (whole.info.bits_allocated, whole.info.samples_per_pixel),
        (8, 1)
    );
    let raw = whole.into_raw_tile((30, 20)).unwrap();
    assert_eq!(
        raw.tile_region(),
        crate::core::types::PixelRect::new(21, 40, 30, 20)
    );
    assert_eq!(raw.data(), source.as_slice());

    // Rectangles are still checked against the image.
    let reason = unsupported_reason(crop_or_whole(source.clone(), (WIDTH, 0, 1, 1), BUDGET));
    assert!(reason.contains("lies outside the 100x76 image"), "{reason}");
    let reason = unsupported_reason(crop_or_whole(source, (0, 0, 0, 1), BUDGET));
    assert!(reason.contains("nonzero rectangle"), "{reason}");
}

#[test]
fn malformed_jpegs_are_errors_not_whole_fallbacks() {
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);
    let mut zero_quantizer = source.clone();
    let dqt = zero_quantizer
        .windows(2)
        .position(|marker| marker == [0xFF, 0xDB])
        .unwrap();
    zero_quantizer[dqt + 5] = 0;
    for (input, expected) in [
        (zero_quantizer, "zero quantization value in table 0"),
        (
            with_component_ids(&source, [1, 1, 3]),
            "duplicate scan component id 1",
        ),
    ] {
        // Neither a short budget nor a full one hands these on whole.
        for budget in [0, BUDGET] {
            match crop_or_whole(input.clone(), (8, 8, 16, 16), budget) {
                Err(WsiError::Jpeg(reason)) => assert!(reason.contains(expected), "{reason}"),
                other => panic!(
                    "expected a malformed-JPEG error for {expected}, got {:?}",
                    other.map(|jpeg| jpeg.tile_region)
                ),
            }
        }
    }

    // More blocks per MCU than JPEG allows: j2k-jpeg parses these headers,
    // and does not extract the second, whose factors do not divide evenly.
    // A short budget must not return either whole.
    for (luma, chroma, blocks) in [(0x44, 0x11, 18), (0x22, 0x33, 22)] {
        let mut oversized = patched_sof(&source, None, Some((7, luma)));
        oversized = patched_sof(&oversized, None, Some((10, chroma)));
        oversized = patched_sof(&oversized, None, Some((13, chroma)));
        assert!(j2k_jpeg::Decoder::inspect(&oversized).is_ok());
        for budget in [0, BUDGET] {
            match crop_or_whole(oversized.clone(), (8, 8, 16, 16), budget) {
                Err(WsiError::Jpeg(reason)) if budget == 0 => assert!(
                    reason.contains(&format!("at most 10 blocks per MCU, got {blocks}")),
                    "{reason}"
                ),
                Err(WsiError::Jpeg(_)) => {}
                other => panic!(
                    "expected a malformed-JPEG error for {blocks} blocks per MCU, got {:?}",
                    other.map(|jpeg| jpeg.tile_region)
                ),
            }
        }
        let reason = match lossless_crop_jpeg_stack(&[&oversized], (8, 8, 16, 16), BUDGET) {
            Err(WsiError::Jpeg(reason)) => reason,
            other => panic!("{:?}", other.map(|jpeg| jpeg.tile_region)),
        };
        assert!(reason.contains("at most 10 blocks per MCU"), "{reason}");
    }
}

#[test]
fn whole_jpeg_fallback_reports_the_crop_frame_and_color() {
    let samples = encode(ColorType::Ycbcr, SamplingFactor::F_1_1);
    let mut adobe = crate::test_support::jpeg_without_jfif(&samples);
    adobe.splice(2..2, ADOBE_RGB);
    let sources = [
        encode(ColorType::Rgb, SamplingFactor::F_2_2),
        adobe,
        with_component_ids(&crate::test_support::jpeg_without_jfif(&samples), *b"RGB"),
        encode(ColorType::Luma, SamplingFactor::F_1_1),
    ];
    for source in sources {
        // An unaligned rectangle running past the far edges.
        let rect = (21, 40, 200, 200);
        let crop = crop_or_whole(source.clone(), rect, BUDGET).unwrap();
        let whole = crop_or_whole(source.clone(), rect, 0).unwrap();
        assert_whole_jpeg(&whole, &source, rect);
        assert_eq!(
            whole.info.photometric_interpretation,
            crop.info.photometric_interpretation
        );
        assert_eq!(whole.info.samples_per_pixel, crop.info.samples_per_pixel);
        let (_, _, width, height) = crop.tile_region;
        assert_eq!(whole.tile_region, (21, 40, width, height));
    }

    // The rectangle is validated as for a crop.
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);
    for rect in [(0, 0, 0, 8), (WIDTH, 0, 1, 1), (10, 0, u32::MAX, 1)] {
        assert!(matches!(
            crop_or_whole(source.clone(), rect, 0),
            Err(WsiError::Unsupported { .. })
        ));
    }
}

#[test]
fn stacked_crops_over_their_coefficient_budget_fail_before_decoding() {
    let (mut parts, _) = stacked_parts(64, &[32, 32, 32], SamplingFactor::F_2_2);
    let rect = (0, 16, 64, 32);
    // The crop takes MCU row 1 of part 0 and MCU row 0 of part 1. A stack is
    // bounded by the cropped window's blocks plus the extraction of its
    // largest touched part, not the sum of the parts'.
    let window = 4 * 2 * 6 * 128;
    let part_window = |row| McuRect {
        x: 0,
        y: row,
        width: 4,
        height: 1,
    };
    let part = extraction_estimate(&parts[0], part_window(1))
        .max(extraction_estimate(&parts[1], part_window(0)));
    assert!((window / 2..window).contains(&part), "{part}");
    let needed = window + part;
    let over_budget = |parts: &[Vec<u8>]| {
        let refs = parts.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let err = lossless_crop_jpeg_stack(&refs, rect, needed - 1).unwrap_err();
        assert!(
            matches!(
                err,
                WsiError::ResourceLimit {
                    resource: "lossless JPEG crop DCT coefficients",
                    requested,
                    limit,
                } if requested == needed && limit == needed - 1
            ),
            "{err:?}"
        );
        lossless_crop_jpeg_stack(&refs, rect, needed)
    };
    over_budget(&parts).expect("one part at a time");

    // The headers decide: a corrupt scan in a touched part is reached only
    // once the budget admits the crop.
    parts[1] = with_truncated_scan(&parts[1]);
    let err = over_budget(&parts).unwrap_err();
    assert!(
        matches!(&err, WsiError::Jpeg(reason) if reason.contains("stack part 1")),
        "{err:?}"
    );
}

#[test]
fn crop_rejects_truncated_and_garbage_input() {
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);
    let sos = first_scan_start(&source);
    let garbage = (0..512u32).map(|i| (i * 97 + 13) as u8).collect::<Vec<_>>();
    let mut soi_garbage = vec![0xFF, 0xD8];
    soi_garbage.extend_from_slice(&garbage);
    let inputs: [(&str, Vec<u8>); 7] = [
        ("empty", Vec::new()),
        ("soi only", vec![0xFF, 0xD8]),
        ("garbage", garbage),
        ("soi + garbage", soi_garbage),
        ("header cut", source[..sos / 2].to_vec()),
        ("scan cut", source[..sos + 40].to_vec()),
        ("scan cut with eoi", with_truncated_scan(&source)),
    ];
    for (label, input) in inputs {
        let result = crop_or_whole(input.clone(), (0, 0, 16, 16), BUDGET);
        assert!(
            matches!(
                result,
                Err(WsiError::Jpeg(_) | WsiError::Unsupported { .. })
            ),
            "{label}: {:?}",
            result.map(|crop| crop.tile_region)
        );
    }
}

/// Overwrite the SOF marker byte, or the byte at `payload_offset` inside SOF.
fn patched_sof(jpeg: &[u8], marker: Option<u8>, payload_byte: Option<(usize, u8)>) -> Vec<u8> {
    let mut out = jpeg.to_vec();
    let sof = jpeg_segments_through_first_scan(jpeg)
        .into_iter()
        .find(|segment| segment.marker == 0xC0)
        .expect("no SOF0");
    if let Some(marker) = marker {
        out[sof.start + 1] = marker;
    }
    if let Some((idx, value)) = payload_byte {
        out[sof.start + 4 + idx] = value;
    }
    out
}

#[test]
fn crop_rejects_unsupported_jpeg_processes_precisely() {
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);
    let cmyk = encode_with(32, 32, ColorType::Cmyk, SamplingFactor::F_1_1, |_| {});
    // The reason names the coding in j2k-jpeg's words.
    for (input, expected) in [
        (patched_sof(&source, Some(0xC1), None), "Extended8"),
        (patched_sof(&source, Some(0xC3), None), "lossless predictor"),
        (patched_sof(&source, Some(0xC9), None), "ArithmeticCoding"),
        (
            patched_sof(&source, Some(0xC5), None),
            "DifferentialBaseline",
        ),
        (patched_sof(&source, None, Some((0, 12))), "bit depth: 12"),
        (cmyk, "Cmyk"),
    ] {
        let reason = unsupported_reason(crop_or_whole(input.clone(), (0, 0, 16, 16), BUDGET));
        assert!(
            reason.starts_with("lossless JPEG crop does not cut this JPEG")
                && reason.contains(expected),
            "{reason}"
        );
    }
}

/// Split a JPEG into a TIFF-style abbreviated tile and a tables stream.
fn abbreviated(jpeg: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut tile = vec![0xFF, 0xD8];
    let mut tables = vec![0xFF, 0xD8];
    for segment in jpeg_segments_through_first_scan(jpeg) {
        let bytes = &jpeg[segment.start..segment.end];
        match segment.marker {
            0xDB | 0xC4 => tables.extend_from_slice(bytes),
            0xDA => tile.extend_from_slice(&jpeg[segment.start..]),
            _ => tile.extend_from_slice(bytes),
        }
    }
    tables.extend_from_slice(&[0xFF, 0xD9]);
    (tile, tables)
}

#[test]
fn abbreviated_tiles_become_standalone_and_crop_like_the_original() {
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);
    let (tile, tables) = abbreviated(&source);
    assert!(crop_or_whole(tile.clone(), (0, 0, 16, 16), BUDGET).is_err());

    let standalone = merge_missing_jpeg_tables(tile.clone(), Some(&tables)).unwrap();
    assert_eq!(
        standalone,
        standalone_jpeg_frame_owned(tile.clone(), Some(&tables))
            .unwrap()
            .0
    );
    assert_eq!(dct(&standalone), dct(&source));
    let from_tiff = crop_or_whole(standalone.clone(), (5, 9, 40, 30), BUDGET).unwrap();
    let direct = crop_or_whole(source.clone(), (5, 9, 40, 30), BUDGET).unwrap();
    assert_eq!(from_tiff.data, direct.data);

    assert_eq!(
        merge_missing_jpeg_tables(source.clone(), None).unwrap(),
        source,
        "complete JPEGs pass through unchanged"
    );
    assert!(merge_missing_jpeg_tables(tile, None).is_err());
}

#[test]
fn abbreviated_progressive_tiles_take_their_tables_without_a_baseline_frame() {
    let source = encode_with(WIDTH, HEIGHT, ColorType::Rgb, SamplingFactor::F_2_2, |e| {
        e.set_progressive(true)
    });
    let (tile, tables) = abbreviated(&source);
    assert!(standalone_jpeg_frame_owned(tile.clone(), Some(&tables)).is_err());

    let standalone = merge_missing_jpeg_tables(tile, Some(&tables)).unwrap();
    assert_eq!(dct(&standalone), dct(&source));
}

#[test]
fn coefficient_budget_sets_aside_the_source_its_marker_copies_and_the_output() {
    let output = lossless_crop_output_bound(1_000, 64, 64);
    assert_eq!(
        lossless_crop_coefficient_budget(1_000_000, 1_000, (64, 64)),
        1_000_000 - 2 * 1_000 - output
    );
    assert_eq!(lossless_crop_coefficient_budget(output, 1_000, (64, 64)), 0);

    // What remains is what j2k-jpeg's estimate is admitted against: a working
    // set one byte short of the set-aside amounts plus the estimate returns
    // the whole JPEG.
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);
    let rect = PixelRect::new(16, 16, 32, 32);
    let source_len = source.len() as u64;
    let set_aside = 2 * source_len + lossless_crop_output_bound(source_len, 32, 32);
    let needed = crop_estimate(&source, (16, 16, 32, 32));
    let crop = lossless_crop_raw_tile(source.clone(), rect, set_aside + needed, u64::MAX).unwrap();
    assert_eq!((crop.width(), crop.height()), (32, 32));
    let whole =
        lossless_crop_raw_tile(source.clone(), rect, set_aside + needed - 1, u64::MAX).unwrap();
    assert_eq!(whole.data(), source.as_slice());
    assert_eq!(whole.tile_region(), rect);
}

#[test]
fn reserved_coefficient_bound_covers_the_estimate_of_every_source_kind() {
    // Admission reserves `lossless_crop_coefficient_bound` from the declared
    // sizes before a source is read. The crop is then gated by j2k-jpeg's
    // estimate from the source's headers, which must never exceed it.
    let restart = |e: &mut Encoder<&mut Vec<u8>>| e.set_restart_interval(1);
    let progressive = |e: &mut Encoder<&mut Vec<u8>>| e.set_progressive(true);
    let multi_scan = |e: &mut Encoder<&mut Vec<u8>>| e.set_optimized_huffman_tables(true);
    type Configure = fn(&mut Encoder<&mut Vec<u8>>);
    let kinds: [(&str, ColorType, SamplingFactor, Configure); 9] = [
        (
            "baseline 4:4:4",
            ColorType::Rgb,
            SamplingFactor::F_1_1,
            |_| {},
        ),
        (
            "baseline 4:2:2",
            ColorType::Rgb,
            SamplingFactor::F_2_1,
            |_| {},
        ),
        (
            "baseline 4:2:0",
            ColorType::Rgb,
            SamplingFactor::F_2_2,
            |_| {},
        ),
        (
            "baseline gray",
            ColorType::Luma,
            SamplingFactor::F_1_1,
            |_| {},
        ),
        (
            "restart 4:4:4",
            ColorType::Rgb,
            SamplingFactor::F_1_1,
            restart,
        ),
        (
            "restart gray",
            ColorType::Luma,
            SamplingFactor::F_1_1,
            restart,
        ),
        (
            "progressive 4:2:0",
            ColorType::Rgb,
            SamplingFactor::F_2_2,
            progressive,
        ),
        (
            "progressive gray",
            ColorType::Luma,
            SamplingFactor::F_1_1,
            progressive,
        ),
        (
            "multi-scan 4:4:4",
            ColorType::Rgb,
            SamplingFactor::F_1_1,
            multi_scan,
        ),
    ];
    for (label, color, sampling, configure) in kinds {
        for (width, height) in [(1, 1), (8, 8), (WIDTH, HEIGHT), (257, 131)] {
            let source = encode_with(width, height, color, sampling, configure);
            let rects = [
                (0, 0, 1, 1),
                (0, 0, width, height),
                (width / 2, height / 2, width, height),
                (width - 1, height - 1, 1, 1),
                (width / 3, 0, width / 2 + 1, height),
            ];
            for rect in rects {
                let (_, _, w, h) = rect;
                let reserved = lossless_crop_coefficient_bound((width, height), (w, h));
                let needed = crop_estimate(&source, rect);
                assert!(
                    needed <= reserved,
                    "{label} {width}x{height} {rect:?}: estimate {needed} > reserved {reserved}"
                );
                // The reservation therefore always admits the crop.
                let crop = crop_or_whole(source.clone(), rect, reserved).unwrap();
                assert_ne!(crop.data, source, "{label} {width}x{height} {rect:?}");
            }
        }
    }

    // The most j2k-jpeg estimates per block: a lone component, so one block
    // per MCU, with whole-image accumulation and a restart index entry for
    // every MCU. (It does not extract such a stream; the estimate still
    // gates it.)
    let dense = encode_with(257, 131, ColorType::Luma, SamplingFactor::F_1_1, |e| {
        e.set_progressive(true);
        e.set_restart_interval(1);
    });
    for rect in [(0, 0, 1, 1), (0, 0, 257, 131), (128, 64, 257, 131)] {
        let reserved = lossless_crop_coefficient_bound((257, 131), (rect.2, rect.3));
        let needed = crop_estimate(&dense, rect);
        assert!(needed <= reserved, "{rect:?}: {needed} > {reserved}");
    }
}

#[test]
fn valid_sources_the_crop_cannot_cut_return_whole_and_corrupt_ones_fail() {
    let rect = (8, 8, 16, 16);
    // j2k-jpeg has no restart index for a progressive stream, so its
    // coefficients are not extracted; the stored JPEG is still a valid answer.
    let progressive_restart =
        encode_with(WIDTH, HEIGHT, ColorType::Rgb, SamplingFactor::F_2_2, |e| {
            e.set_progressive(true);
            e.set_restart_interval(2);
        });
    assert!(matches!(
        extract_dct_blocks(&progressive_restart, DctExtractOptions::quantized_only()),
        Err(j2k_jpeg::JpegError::NotImplemented { .. })
    ));
    let whole = crop_or_whole(progressive_restart.clone(), rect, BUDGET).unwrap();
    assert_whole_jpeg_headers(&whole, &progressive_restart, rect);

    // The same classification never lets a corrupt stream through: with the
    // budget to decode it, it is an error.
    let source = encode(ColorType::Rgb, SamplingFactor::F_2_2);
    let mut corrupt = source[..source.len() / 2].to_vec();
    corrupt.extend_from_slice(&[0xFF, 0xD9]);
    let result = crop_or_whole(corrupt, (0, 0, WIDTH, HEIGHT), BUDGET);
    assert!(
        matches!(result, Err(WsiError::Jpeg(_))),
        "{:?}",
        result.map(|jpeg| jpeg.tile_region)
    );
}

/// [`assert_whole_jpeg`] for a source whose coefficients j2k-jpeg does not
/// extract: the expected frame comes from its headers.
fn assert_whole_jpeg_headers(result: &LosslessJpeg, source: &[u8], rect: (u32, u32, u32, u32)) {
    let info = j2k_jpeg::Decoder::inspect(source).unwrap();
    let (x, y, w, h) = rect;
    assert_eq!(result.data, source, "the stored JPEG comes back unchanged");
    assert_eq!((result.width, result.height), info.dimensions);
    assert_eq!(
        result.tile_region,
        (
            x,
            y,
            (x + w).min(info.dimensions.0) - x,
            (y + h).min(info.dimensions.1) - y
        )
    );
    assert_eq!(
        usize::from(result.info.samples_per_pixel),
        info.sampling.len()
    );
}
