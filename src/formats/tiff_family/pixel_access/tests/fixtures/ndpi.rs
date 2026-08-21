use super::super::*;
use super::layout::*;
use super::tiff::*;

pub(in super::super) fn build_test_ndpi_reader_for_strip_cache(
    width: u32,
    height: u32,
    tiles_across: u32,
) -> (TiffPixelReader, IfdId) {
    let tiles_down = height.div_ceil(16);
    let jpeg = encode_restart_rgb_jpeg(
        &image::RgbImage::from_pixel(width, height, image::Rgb([0, 0, 0])),
        75,
        8,
    );
    let bitstream_start = find_test_jpeg_bitstream_start(&jpeg).unwrap();
    let jpeg_header = jpeg[..bitstream_start].to_vec();
    let file =
        build_ndpi_full_jpeg_tiff(width, height, &jpeg, (tiles_across * tiles_down) as usize);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let dimensions = (u64::from(width), u64::from(height));
    let layout = single_series_layout(
        DatasetId::new(12),
        vec![
            whole_level(dimensions, 1.0, (128, 16)),
            whole_level(dimensions, 2.0, (128, 16)),
        ],
        HashMap::from([(
            tile_source_key(1),
            TileSource::NdpiJpeg {
                ifd_id,
                jpeg_header,
                mcu_starts_tag: 65426,
                tiles_across,
                tiles_down,
                restart_interval: 8,
                strip_offset: 8,
                strip_byte_count: jpeg.len() as u64,
            },
        )]),
    );
    (TiffPixelReader::new(container, layout), ifd_id)
}

pub(in super::super) fn encode_restart_rgb_jpeg(
    image: &image::RgbImage,
    quality: u8,
    restart_interval: u16,
) -> Vec<u8> {
    let mut encoded = Vec::new();
    let mut encoder = JpegEncoder::new(&mut encoded, quality);
    encoder.set_restart_interval(restart_interval);
    encoder
        .encode(
            image.as_raw().as_slice(),
            image.width() as u16,
            image.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    encoded
}

pub(in super::super) fn find_test_jpeg_bitstream_start(data: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < data.len().saturating_sub(1) {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if marker == 0xD8 || marker == 0x00 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if i + 3 >= data.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if marker == 0xDA {
            return Some(i + 2 + seg_len);
        }
        i += 2 + seg_len;
    }
    None
}

pub(in super::super) fn test_jpeg_restart_segment_starts(data: &[u8]) -> Vec<u32> {
    let mut starts = Vec::new();
    if let Some(entropy_start) = find_test_jpeg_bitstream_start(data) {
        starts.push(entropy_start as u32);
    }
    let mut i = starts.first().copied().unwrap_or(0) as usize;
    while i + 1 < data.len() {
        if data[i] == 0xFF && (0xD0..=0xD7).contains(&data[i + 1]) {
            starts.push(i as u32);
            i += 2;
            continue;
        }
        i += 1;
    }
    starts
}

pub(in super::super) fn zero_test_jpeg_sof_dimensions(data: &mut [u8]) {
    let sof = data
        .windows(2)
        .position(|bytes| bytes == [0xFF, 0xC0])
        .expect("test JPEG has SOF0");
    data[sof + 5..sof + 9].copy_from_slice(&[0, 0, 0, 0]);
}

pub(in super::super) fn finish_ndpi_mcu_tiff(
    mut buf: Vec<u8>,
    first_ifd_pos: usize,
    width: u32,
    height: u32,
    strip_offset: u32,
    strip_byte_count: u32,
    mcu_starts: &[u32],
) -> NamedTempFile {
    let mcu_starts_array_offset = append_optional_u32_array(&mut buf, mcu_starts);

    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&le_u32(ifd_offset));

    append_ifd_tags(
        &mut buf,
        vec![
            (256u16, 4u16, 1u32, le_u32(width)),
            (257u16, 4u16, 1u32, le_u32(height)),
            (259u16, 3u16, 1u32, short_in_u32(7)),
            (262u16, 3u16, 1u32, short_in_u32(6)),
            (273u16, 4u16, 1u32, le_u32(strip_offset)),
            (277u16, 3u16, 1u32, short_in_u32(3)),
            (279u16, 4u16, 1u32, le_u32(strip_byte_count)),
            (
                65426u16,
                4u16,
                mcu_starts.len() as u32,
                u32_array_offset_or_inline_value(mcu_starts, mcu_starts_array_offset),
            ),
        ],
    );

    temp_tiff_from_buffer(&buf)
}

pub(in super::super) fn build_ndpi_full_jpeg_tiff(
    width: u32,
    height: u32,
    jpeg_data: &[u8],
    blob_count: usize,
) -> NamedTempFile {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&le_u32(0));

    let strip_offset = buf.len() as u32;
    let mut mcu_starts = test_jpeg_restart_segment_starts(jpeg_data);
    if mcu_starts.len() >= blob_count {
        mcu_starts.truncate(blob_count);
    } else {
        mcu_starts = (0..blob_count as u32).collect();
    }
    buf.extend_from_slice(jpeg_data);
    let strip_byte_count = buf.len() as u32 - strip_offset;

    finish_ndpi_mcu_tiff(
        buf,
        first_ifd_pos,
        width,
        height,
        strip_offset,
        strip_byte_count,
        &mcu_starts,
    )
}

#[derive(Clone, Copy)]
pub(in super::super) enum TestMcuStartsMode {
    Relative,
    FileAbsolute,
    InvalidFileAbsolute,
}

pub(in super::super) fn build_ndpi_scan_data_tiff_from_blobs(
    width: u32,
    height: u32,
    colors: &[[u8; 3]],
    zero_sof_dimensions: bool,
) -> (NamedTempFile, Vec<u8>, u64) {
    build_ndpi_scan_data_tiff_from_blobs_with_mcu_mode(
        width,
        height,
        colors,
        zero_sof_dimensions,
        TestMcuStartsMode::Relative,
    )
}

pub(in super::super) fn build_ndpi_scan_data_tiff_from_blobs_with_mcu_mode(
    width: u32,
    height: u32,
    colors: &[[u8; 3]],
    zero_sof_dimensions: bool,
    mcu_mode: TestMcuStartsMode,
) -> (NamedTempFile, Vec<u8>, u64) {
    let (file, jpeg_header, strip_byte_count, _) =
        build_ndpi_scan_data_tiff_from_blobs_with_mcu_mode_and_offset(
            width,
            height,
            colors,
            zero_sof_dimensions,
            mcu_mode,
        );
    (file, jpeg_header, strip_byte_count)
}

pub(in super::super) fn build_ndpi_scan_data_tiff_from_blobs_with_mcu_mode_and_offset(
    width: u32,
    height: u32,
    colors: &[[u8; 3]],
    zero_sof_dimensions: bool,
    mcu_mode: TestMcuStartsMode,
) -> (NamedTempFile, Vec<u8>, u64, u64) {
    let test_tile_width = 64;
    let test_tile_height = 8;
    let tiles_across = width.div_ceil(test_tile_width);
    let mut image = image::RgbImage::new(width, height);
    for (idx, rgb) in colors.iter().enumerate() {
        let tile_col = (idx as u32) % tiles_across;
        let tile_row = (idx as u32) / tiles_across;
        let x0 = tile_col * test_tile_width;
        let y0 = tile_row * test_tile_height;
        for y in y0..(y0 + test_tile_height).min(height) {
            for x in x0..(x0 + test_tile_width).min(width) {
                image.put_pixel(x, y, image::Rgb(*rgb));
            }
        }
    }
    let mut encoded = encode_restart_rgb_jpeg(&image, 95, 8);
    if zero_sof_dimensions {
        zero_test_jpeg_sof_dimensions(&mut encoded);
    }
    let bitstream_start = find_test_jpeg_bitstream_start(&encoded).unwrap();
    let jpeg_header = encoded[..bitstream_start].to_vec();
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&le_u32(0));
    if matches!(
        mcu_mode,
        TestMcuStartsMode::FileAbsolute | TestMcuStartsMode::InvalidFileAbsolute
    ) {
        buf.resize(4096, 0);
    }

    let strip_offset = buf.len() as u32;
    let mut mcu_starts = test_jpeg_restart_segment_starts(&encoded);
    mcu_starts.truncate(colors.len());
    assert_eq!(mcu_starts.len(), colors.len());
    buf.extend_from_slice(&encoded);
    let strip_byte_count = buf.len() as u32 - strip_offset;
    match mcu_mode {
        TestMcuStartsMode::Relative => {}
        TestMcuStartsMode::FileAbsolute => {
            for value in &mut mcu_starts {
                *value = value.saturating_add(strip_offset);
            }
        }
        TestMcuStartsMode::InvalidFileAbsolute => {
            for value in &mut mcu_starts {
                *value = value
                    .saturating_add(strip_offset)
                    .saturating_add(strip_byte_count)
                    .saturating_add(8);
            }
        }
    }

    let file = finish_ndpi_mcu_tiff(
        buf,
        first_ifd_pos,
        width,
        height,
        strip_offset,
        strip_byte_count,
        &mcu_starts,
    );
    (
        file,
        jpeg_header,
        strip_byte_count as u64,
        strip_offset as u64,
    )
}

// ── TiffPixelReader tests ─────────────────────────────────────

// Note: Testing TiffPixelReader with NdpiJpeg requires a synthetic NDPI
// file with valid MCU-starts tags. Since building such files is complex,
// we test the TiffPixelReader through the full interpret -> read path in
// Task 9's integration tests. Here we test the FullDecodeCache directly
// (above) and add integration tests in Task 9.
