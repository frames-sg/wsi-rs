use super::super::*;

pub(in super::super) fn le_u16(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

pub(in super::super) fn le_u32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

pub(in super::super) fn short_in_u32(v: u16) -> [u8; 4] {
    let mut bytes = [0u8; 4];
    bytes[..2].copy_from_slice(&le_u16(v));
    bytes
}

type TiffTagForTest = (u16, u16, u32, [u8; 4]);

pub(in super::super) fn append_u32_array(buf: &mut Vec<u8>, values: &[u32]) -> u32 {
    let offset = buf.len() as u32;
    for value in values {
        buf.extend_from_slice(&le_u32(*value));
    }
    offset
}

pub(in super::super) fn append_optional_u32_array(
    buf: &mut Vec<u8>,
    values: &[u32],
) -> Option<u32> {
    (values.len() > 1).then(|| append_u32_array(buf, values))
}

pub(in super::super) fn u32_array_offset_or_inline_value(
    values: &[u32],
    array_offset: Option<u32>,
) -> [u8; 4] {
    array_offset
        .map(le_u32)
        .unwrap_or_else(|| le_u32(values[0]))
}

pub(in super::super) fn append_ifd_tags(buf: &mut Vec<u8>, mut tags: Vec<TiffTagForTest>) {
    tags.sort_by_key(|tag| tag.0);

    buf.extend_from_slice(&le_u16(tags.len() as u16));
    for (tag, typ, count, value) in &tags {
        buf.extend_from_slice(&le_u16(*tag));
        buf.extend_from_slice(&le_u16(*typ));
        buf.extend_from_slice(&le_u32(*count));
        buf.extend_from_slice(value);
    }
    buf.extend_from_slice(&le_u32(0));
}

pub(in super::super) fn temp_tiff_from_buffer(buf: &[u8]) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(buf).unwrap();
    file.flush().unwrap();
    file
}

pub(in super::super) fn build_tiled_associated_tiff(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    tiles: &[Vec<u8>],
) -> NamedTempFile {
    build_tiled_encoded_tiff(width, height, tile_width, tile_height, tiles, 1, 1, 1)
}

#[allow(clippy::too_many_arguments)]
pub(in super::super) fn build_tiled_encoded_tiff(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    tiles: &[Vec<u8>],
    compression_tag: u16,
    samples_per_pixel: u16,
    photometric: u16,
) -> NamedTempFile {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&le_u32(0));

    let mut tile_offsets = Vec::with_capacity(tiles.len());
    let mut tile_byte_counts = Vec::with_capacity(tiles.len());
    for tile in tiles {
        tile_offsets.push(buf.len() as u32);
        tile_byte_counts.push(tile.len() as u32);
        buf.extend_from_slice(tile);
    }

    let tile_offsets_array_offset = append_optional_u32_array(&mut buf, &tile_offsets);
    let tile_byte_counts_array_offset = append_optional_u32_array(&mut buf, &tile_byte_counts);

    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&le_u32(ifd_offset));

    append_ifd_tags(
        &mut buf,
        vec![
            (256u16, 4u16, 1u32, le_u32(width)),
            (257u16, 4u16, 1u32, le_u32(height)),
            (258u16, 3u16, 1u32, short_in_u32(8)),
            (259u16, 3u16, 1u32, short_in_u32(compression_tag)),
            (262u16, 3u16, 1u32, short_in_u32(photometric)),
            (277u16, 3u16, 1u32, short_in_u32(samples_per_pixel)),
            (322u16, 4u16, 1u32, le_u32(tile_width)),
            (323u16, 4u16, 1u32, le_u32(tile_height)),
            (
                324u16,
                4u16,
                tile_offsets.len() as u32,
                u32_array_offset_or_inline_value(&tile_offsets, tile_offsets_array_offset),
            ),
            (
                325u16,
                4u16,
                tile_byte_counts.len() as u32,
                u32_array_offset_or_inline_value(&tile_byte_counts, tile_byte_counts_array_offset),
            ),
        ],
    );

    temp_tiff_from_buffer(&buf)
}

pub(in super::super) fn build_stripped_jpeg_tiff(
    width: u32,
    height: u32,
    jpeg_data: &[u8],
) -> NamedTempFile {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&le_u32(0));

    let strip_offset = buf.len() as u32;
    buf.extend_from_slice(jpeg_data);
    let strip_byte_count = jpeg_data.len() as u32;

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
        ],
    );

    temp_tiff_from_buffer(&buf)
}

pub(in super::super) fn build_stripped_uncompressed_tiff(
    width: u32,
    height: u32,
    pixels: &[u8],
    samples_per_pixel: u16,
    photometric: Option<u16>,
) -> NamedTempFile {
    build_stripped_uncompressed_tiff_with_predictor(
        width,
        height,
        pixels,
        samples_per_pixel,
        photometric,
        None,
    )
}

pub(in super::super) fn build_stripped_uncompressed_tiff_with_predictor(
    width: u32,
    height: u32,
    pixels: &[u8],
    samples_per_pixel: u16,
    photometric: Option<u16>,
    predictor: Option<u16>,
) -> NamedTempFile {
    build_stripped_tiff(
        width,
        height,
        pixels,
        samples_per_pixel,
        photometric,
        predictor,
        1,
    )
}

pub(in super::super) fn build_stripped_tiff(
    width: u32,
    height: u32,
    payload: &[u8],
    samples_per_pixel: u16,
    photometric: Option<u16>,
    predictor: Option<u16>,
    compression: u16,
) -> NamedTempFile {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&le_u32(0));

    let strip_offset = buf.len() as u32;
    buf.extend_from_slice(payload);
    let strip_byte_count = payload.len() as u32;

    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&le_u32(ifd_offset));

    let mut tags = vec![
        (256u16, 4u16, 1u32, le_u32(width)),
        (257u16, 4u16, 1u32, le_u32(height)),
        (258u16, 3u16, 1u32, short_in_u32(8)),
        (259u16, 3u16, 1u32, short_in_u32(compression)),
        (273u16, 4u16, 1u32, le_u32(strip_offset)),
        (277u16, 3u16, 1u32, short_in_u32(samples_per_pixel)),
        (279u16, 4u16, 1u32, le_u32(strip_byte_count)),
    ];
    if let Some(value) = photometric {
        tags.push((262u16, 3u16, 1u32, short_in_u32(value)));
    }
    if let Some(value) = predictor {
        tags.push((317u16, 3u16, 1u32, short_in_u32(value)));
    }
    append_ifd_tags(&mut buf, tags);

    temp_tiff_from_buffer(&buf)
}

pub(in super::super) fn build_multi_stripped_jpeg_tiff(
    width: u32,
    height: u32,
    rows_per_strip: u32,
    strips: &[Vec<u8>],
) -> NamedTempFile {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&le_u32(0));

    let mut strip_offsets = Vec::with_capacity(strips.len());
    let mut strip_byte_counts = Vec::with_capacity(strips.len());
    for strip in strips {
        strip_offsets.push(buf.len() as u32);
        buf.extend_from_slice(strip);
        strip_byte_counts.push(strip.len() as u32);
    }

    let strip_offsets_array_offset = append_u32_array(&mut buf, &strip_offsets);
    let strip_byte_counts_array_offset = append_u32_array(&mut buf, &strip_byte_counts);

    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&le_u32(ifd_offset));

    append_ifd_tags(
        &mut buf,
        vec![
            (256u16, 4u16, 1u32, le_u32(width)),
            (257u16, 4u16, 1u32, le_u32(height)),
            (259u16, 3u16, 1u32, short_in_u32(7)),
            (262u16, 3u16, 1u32, short_in_u32(6)),
            (
                273u16,
                4u16,
                strip_offsets.len() as u32,
                le_u32(strip_offsets_array_offset),
            ),
            (277u16, 3u16, 1u32, short_in_u32(3)),
            (278u16, 4u16, 1u32, le_u32(rows_per_strip)),
            (
                279u16,
                4u16,
                strip_byte_counts.len() as u32,
                le_u32(strip_byte_counts_array_offset),
            ),
        ],
    );

    temp_tiff_from_buffer(&buf)
}

pub(in super::super) fn encode_solid_rgb_jpeg(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb(rgb));
    let mut encoded = Vec::new();
    JpegEncoder::new(&mut encoded, 95)
        .encode(
            image.as_raw().as_slice(),
            image.width() as u16,
            image.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    encoded
}

pub(in super::super) fn build_tiled_jpeg_reader(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    tiles: &[Vec<u8>],
) -> TiffPixelReader {
    let file = build_tiled_associated_tiff(width, height, tile_width, tile_height, tiles);
    build_tiled_reader_from_file(
        file,
        width,
        height,
        tile_width,
        tile_height,
        DatasetId::new(31),
        Compression::Jpeg,
        None,
    )
}

pub(in super::super) fn build_tiled_jpeg_reader_with_tables(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    tiles: &[Vec<u8>],
    jpeg_tables: Vec<u8>,
) -> TiffPixelReader {
    let file = build_tiled_jpeg_tiff_with_tables(
        width,
        height,
        tile_width,
        tile_height,
        tiles,
        &jpeg_tables,
    );
    build_tiled_reader_from_file(
        file,
        width,
        height,
        tile_width,
        tile_height,
        DatasetId::new(32),
        Compression::Jpeg,
        Some(jpeg_tables),
    )
}

#[allow(clippy::too_many_arguments)]
pub(in super::super) fn build_tiled_encoded_reader(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    tiles: &[Vec<u8>],
    compression: Compression,
    compression_tag: u16,
    samples_per_pixel: u16,
    photometric: u16,
) -> TiffPixelReader {
    let file = build_tiled_encoded_tiff(
        width,
        height,
        tile_width,
        tile_height,
        tiles,
        compression_tag,
        samples_per_pixel,
        photometric,
    );
    build_tiled_reader_from_file(
        file,
        width,
        height,
        tile_width,
        tile_height,
        DatasetId::new(33),
        compression,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in super::super) fn build_tiled_reader_from_file(
    file: NamedTempFile,
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    dataset_id: DatasetId,
    compression: Compression,
    jpeg_tables: Option<Vec<u8>>,
) -> TiffPixelReader {
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = single_series_layout(
        dataset_id,
        vec![regular_level(width, height, tile_width, tile_height)],
        HashMap::from([(
            tile_source_key(0),
            TileSource::TiledIfd {
                ifd_id,
                jpeg_tables,
                compression,
            },
        )]),
    );
    TiffPixelReader::new(container, layout)
}

pub(in super::super) fn build_tiled_jpeg_tiff_with_tables(
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    tiles: &[Vec<u8>],
    jpeg_tables: &[u8],
) -> NamedTempFile {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&le_u16(42));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&le_u32(0));

    let mut tile_offsets = Vec::with_capacity(tiles.len());
    let mut tile_byte_counts = Vec::with_capacity(tiles.len());
    for tile in tiles {
        tile_offsets.push(buf.len() as u32);
        tile_byte_counts.push(tile.len() as u32);
        buf.extend_from_slice(tile);
    }

    let tile_offsets_array_offset = append_optional_u32_array(&mut buf, &tile_offsets);
    let tile_byte_counts_array_offset = append_optional_u32_array(&mut buf, &tile_byte_counts);

    let jpeg_tables_offset = buf.len() as u32;
    buf.extend_from_slice(jpeg_tables);

    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&le_u32(ifd_offset));

    append_ifd_tags(
        &mut buf,
        vec![
            (256u16, 4u16, 1u32, le_u32(width)),
            (257u16, 4u16, 1u32, le_u32(height)),
            (258u16, 3u16, 1u32, short_in_u32(8)),
            (259u16, 3u16, 1u32, short_in_u32(7)),
            (262u16, 3u16, 1u32, short_in_u32(6)),
            (277u16, 3u16, 1u32, short_in_u32(3)),
            (322u16, 4u16, 1u32, le_u32(tile_width)),
            (323u16, 4u16, 1u32, le_u32(tile_height)),
            (
                324u16,
                4u16,
                tile_offsets.len() as u32,
                u32_array_offset_or_inline_value(&tile_offsets, tile_offsets_array_offset),
            ),
            (
                325u16,
                4u16,
                tile_byte_counts.len() as u32,
                u32_array_offset_or_inline_value(&tile_byte_counts, tile_byte_counts_array_offset),
            ),
            (
                347u16,
                7u16,
                jpeg_tables.len() as u32,
                le_u32(jpeg_tables_offset),
            ),
        ],
    );

    temp_tiff_from_buffer(&buf)
}

pub(in super::super) fn split_test_jpeg_tables(jpeg: &[u8]) -> (Vec<u8>, Vec<u8>) {
    assert!(jpeg.starts_with(&[0xFF, 0xD8]));
    let mut abbreviated = Vec::from(&jpeg[..2]);
    let mut tables = Vec::from(&jpeg[..2]);
    let mut offset = 2usize;
    while offset + 4 <= jpeg.len() {
        assert_eq!(jpeg[offset], 0xFF);
        let marker = jpeg[offset + 1];
        if marker == 0xDA {
            abbreviated.extend_from_slice(&jpeg[offset..]);
            tables.extend_from_slice(&[0xFF, 0xD9]);
            return (abbreviated, tables);
        }
        let len = u16::from_be_bytes([jpeg[offset + 2], jpeg[offset + 3]]) as usize;
        let end = offset + 2 + len;
        assert!(end <= jpeg.len());
        if marker == 0xDB || marker == 0xC4 {
            tables.extend_from_slice(&jpeg[offset..end]);
        } else {
            abbreviated.extend_from_slice(&jpeg[offset..end]);
        }
        offset = end;
    }
    panic!("test JPEG did not contain SOS marker");
}

pub(in super::super) fn load_fixture_rgb(ppm_bytes: &[u8]) -> image::RgbImage {
    match image::load(Cursor::new(ppm_bytes), ImageFormat::Pnm).unwrap() {
        DynamicImage::ImageRgb8(image) => image,
        other => other.to_rgb8(),
    }
}
