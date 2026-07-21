use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};
use std::io::Write;
use tempfile::NamedTempFile;

fn encode_test_jpeg(image: &image::RgbImage) -> Vec<u8> {
    let mut encoded = Vec::new();
    JpegEncoder::new(&mut encoded, 50)
        .encode(
            image.as_raw().as_slice(),
            image.width() as u16,
            image.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    encoded
}

/// Build a synthetic NDPI TIFF file with embedded JPEG data.
/// Each entry is (width, height, source_lens).
pub(super) fn build_ndpi_tiff(entries: &[(u32, u32, f32)]) -> NamedTempFile {
    // Step 1: Build minimal JPEG for each entry
    let mut jpeg_blocks: Vec<Vec<u8>> = Vec::new();
    for &(w, h, _) in entries {
        let actual_w = w.min(64);
        let actual_h = h.min(64);
        let rgb = image::RgbImage::new(actual_w, actual_h);
        jpeg_blocks.push(encode_test_jpeg(&rgb));
    }

    let mut buf = Vec::new();

    // TIFF header: little-endian, classic TIFF
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    let first_ifd_offset_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes());

    // Write JPEG data blocks
    let mut strip_offsets: Vec<u32> = Vec::new();
    let mut strip_byte_counts: Vec<u32> = Vec::new();
    for jpeg in &jpeg_blocks {
        strip_offsets.push(buf.len() as u32);
        strip_byte_counts.push(jpeg.len() as u32);
        buf.extend_from_slice(jpeg);
    }

    // Write IFDs
    let mut ifd_offsets: Vec<u32> = Vec::new();
    let mut next_ifd_patch_positions: Vec<usize> = Vec::new();

    for (i, &(w, h, lens)) in entries.iter().enumerate() {
        let ifd_offset = buf.len() as u32;
        ifd_offsets.push(ifd_offset);

        let mut tags: Vec<(u16, u16, u32, [u8; 4])> = vec![
            (256, 4, 1, w.to_le_bytes()),                    // IMAGE_WIDTH
            (257, 4, 1, h.to_le_bytes()),                    // IMAGE_LENGTH
            (273, 4, 1, strip_offsets[i].to_le_bytes()),     // STRIP_OFFSETS
            (279, 4, 1, strip_byte_counts[i].to_le_bytes()), // STRIP_BYTE_COUNTS
            (65421, 11, 1, lens.to_le_bytes()),              // SOURCELENS (float)
        ];

        // Add NDPI marker tag to first IFD
        if i == 0 {
            tags.push((65420, 4, 1, 1u32.to_le_bytes())); // NDPI marker
        }

        tags.sort_by_key(|t| t.0);

        let entry_count = tags.len() as u16;
        buf.extend_from_slice(&entry_count.to_le_bytes());

        for (tag_id, type_id, count, value) in &tags {
            buf.extend_from_slice(&tag_id.to_le_bytes());
            buf.extend_from_slice(&type_id.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
            buf.extend_from_slice(value);
        }

        // NDPI uses 8-byte next-IFD pointers
        let next_pos = buf.len();
        buf.extend_from_slice(&0u64.to_le_bytes());
        next_ifd_patch_positions.push(next_pos);
    }

    // Patch first IFD offset
    buf[first_ifd_offset_pos..first_ifd_offset_pos + 4]
        .copy_from_slice(&ifd_offsets[0].to_le_bytes());

    // Chain IFDs
    for i in 0..ifd_offsets.len().saturating_sub(1) {
        let next = ifd_offsets[i + 1] as u64;
        let pos = next_ifd_patch_positions[i];
        buf[pos..pos + 8].copy_from_slice(&next.to_le_bytes());
    }

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

/// Build a synthetic tiled TIFF with Aperio-style detection tags.
/// Returns a NamedTempFile with valid JPEG tile data.
pub(super) fn build_aperio_tiff(width: u32, height: u32) -> NamedTempFile {
    let tw = 256u32.min(width);
    let th = 256u32.min(height);
    let rgb = image::RgbImage::new(tw, th);
    let jpeg = encode_test_jpeg(&rgb);

    // Write Aperio ImageDescription as out-of-line ASCII
    let desc = b"Aperio Image Library|AppMag = 40|MPP = 0.25\0";

    let mut buf = Vec::new();
    // TIFF header
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes());

    // Write JPEG tile data
    let tile_offset = buf.len() as u32;
    let tile_byte_count = jpeg.len() as u32;
    buf.extend_from_slice(&jpeg);

    // Write ImageDescription string
    let desc_offset = buf.len() as u32;
    buf.extend_from_slice(desc);

    // Write IFD
    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&ifd_offset.to_le_bytes());

    let mut tags_vec: Vec<(u16, u16, u32, [u8; 4])> = vec![
        (256, 4, 1, width.to_le_bytes()),  // IMAGE_WIDTH
        (257, 4, 1, height.to_le_bytes()), // IMAGE_LENGTH
        (259, 3, 1, {
            // COMPRESSION = JPEG (7)
            let mut v = [0u8; 4];
            v[..2].copy_from_slice(&7u16.to_le_bytes());
            v
        }),
        (270, 2, desc.len() as u32, desc_offset.to_le_bytes()), // IMAGE_DESCRIPTION (out-of-line)
        (322, 4, 1, tw.to_le_bytes()),                          // TILE_WIDTH
        (323, 4, 1, th.to_le_bytes()),                          // TILE_LENGTH
        (324, 4, 1, tile_offset.to_le_bytes()),                 // TILE_OFFSETS
        (325, 4, 1, tile_byte_count.to_le_bytes()),             // TILE_BYTE_COUNTS
    ];
    tags_vec.sort_by_key(|t| t.0);

    buf.extend_from_slice(&(tags_vec.len() as u16).to_le_bytes());
    for (tag, typ, count, val) in &tags_vec {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&typ.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(val);
    }
    buf.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

/// Build a generic tiled TIFF (no vendor-specific tags).
pub(super) fn build_generic_tiled_tiff(width: u32, height: u32) -> NamedTempFile {
    let tw = 256u32.min(width);
    let th = 256u32.min(height);
    let rgb = image::RgbImage::new(tw, th);
    let jpeg = encode_test_jpeg(&rgb);

    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes());

    let tile_offset = buf.len() as u32;
    let tile_byte_count = jpeg.len() as u32;
    buf.extend_from_slice(&jpeg);

    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&ifd_offset.to_le_bytes());

    let mut tags_vec: Vec<(u16, u16, u32, [u8; 4])> = vec![
        (256, 4, 1, width.to_le_bytes()),
        (257, 4, 1, height.to_le_bytes()),
        (259, 3, 1, {
            let mut v = [0u8; 4];
            v[..2].copy_from_slice(&7u16.to_le_bytes());
            v
        }),
        (322, 4, 1, tw.to_le_bytes()),
        (323, 4, 1, th.to_le_bytes()),
        (324, 4, 1, tile_offset.to_le_bytes()),
        (325, 4, 1, tile_byte_count.to_le_bytes()),
    ];
    tags_vec.sort_by_key(|t| t.0);

    buf.extend_from_slice(&(tags_vec.len() as u16).to_le_bytes());
    for (tag, typ, count, val) in &tags_vec {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&typ.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(val);
    }
    buf.extend_from_slice(&0u32.to_le_bytes());

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

/// Build an uncompressed RGB TIFF with separate sample planes and no native tiles.
pub(super) fn build_planar_stripped_rgb_tiff(
    width: u32,
    height: u32,
    rows_per_strip: u32,
) -> NamedTempFile {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes());

    let mut strip_offsets = Vec::new();
    let mut strip_byte_counts = Vec::new();
    for channel in 0..3 {
        for strip_y in (0..height).step_by(rows_per_strip as usize) {
            strip_offsets.push(buf.len() as u32);
            let strip_height = rows_per_strip.min(height - strip_y);
            for y in strip_y..strip_y + strip_height {
                for x in 0..width {
                    let sample = match channel {
                        0 => x as u8,
                        1 => y as u8,
                        _ => x.wrapping_add(y) as u8,
                    };
                    buf.push(sample);
                }
            }
            strip_byte_counts.push(width * strip_height);
        }
    }

    let bits_per_sample_offset = buf.len() as u32;
    for bits in [8u16; 3] {
        buf.extend_from_slice(&bits.to_le_bytes());
    }
    let strip_offsets_offset = buf.len() as u32;
    for offset in &strip_offsets {
        buf.extend_from_slice(&offset.to_le_bytes());
    }
    let strip_byte_counts_offset = buf.len() as u32;
    for byte_count in &strip_byte_counts {
        buf.extend_from_slice(&byte_count.to_le_bytes());
    }

    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&ifd_offset.to_le_bytes());
    let mut tags: Vec<(u16, u16, u32, [u8; 4])> = vec![
        (256, 4, 1, width.to_le_bytes()),
        (257, 4, 1, height.to_le_bytes()),
        (258, 3, 3, bits_per_sample_offset.to_le_bytes()),
        (259, 3, 1, to_short_in_long(1, false)),
        (262, 3, 1, to_short_in_long(2, false)),
        (
            273,
            4,
            strip_offsets.len() as u32,
            strip_offsets_offset.to_le_bytes(),
        ),
        (274, 3, 1, to_short_in_long(1, false)),
        (277, 3, 1, to_short_in_long(3, false)),
        (278, 4, 1, rows_per_strip.to_le_bytes()),
        (
            279,
            4,
            strip_byte_counts.len() as u32,
            strip_byte_counts_offset.to_le_bytes(),
        ),
        (284, 3, 1, to_short_in_long(2, false)),
    ];
    tags.sort_by_key(|tag| tag.0);
    buf.extend_from_slice(&(tags.len() as u16).to_le_bytes());
    for (tag, typ, count, value) in tags {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&typ.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&value);
    }
    buf.extend_from_slice(&0u32.to_le_bytes());

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

// ── Review finding tests ─────────────────────────────────────

/// Build a tiled TIFF with uncompressed RGB data (compression=1).
pub(super) fn build_uncompressed_tiled_tiff(
    width: u32,
    height: u32,
    big_endian: bool,
) -> NamedTempFile {
    let spp: u32 = 3;
    let raw_size = width as usize * height as usize * spp as usize;
    // Write test pattern: pixel (x, y) = (x % 256, y % 256, 128)
    let mut raw = vec![0u8; raw_size];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let idx = (y * width as usize + x) * 3;
            raw[idx] = (x % 256) as u8;
            raw[idx + 1] = (y % 256) as u8;
            raw[idx + 2] = 128;
        }
    }

    let bom: &[u8] = if big_endian { b"MM" } else { b"II" };

    let mut buf = Vec::new();
    buf.extend_from_slice(bom);
    buf.extend_from_slice(&to_bytes_u16(42, big_endian));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&to_bytes_u32(0, big_endian)); // placeholder

    // Write raw tile data
    let tile_offset = buf.len() as u32;
    let tile_byte_count = raw.len() as u32;
    buf.extend_from_slice(&raw);

    // IFD
    let ifd_offset = buf.len() as u32;
    {
        let p = first_ifd_pos;
        let bytes = to_bytes_u32(ifd_offset, big_endian);
        buf[p..p + 4].copy_from_slice(&bytes);
    }

    // Tags: sorted by ID
    let mut tags_data: Vec<(u16, u16, u32, [u8; 4])> = vec![
        (256, 4, 1, to_bytes_u32_arr(width, big_endian)), // IMAGE_WIDTH
        (257, 4, 1, to_bytes_u32_arr(height, big_endian)), // IMAGE_LENGTH
        (258, 3, 1, to_short_in_long(8, big_endian)),     // BITS_PER_SAMPLE
        (259, 3, 1, to_short_in_long(1, big_endian)),     // COMPRESSION = None
        (262, 3, 1, to_short_in_long(2, big_endian)),     // PHOTOMETRIC = RGB
        (277, 3, 1, to_short_in_long(spp as u16, big_endian)), // SAMPLES_PER_PIXEL
        (322, 4, 1, to_bytes_u32_arr(width, big_endian)), // TILE_WIDTH
        (323, 4, 1, to_bytes_u32_arr(height, big_endian)), // TILE_LENGTH
        (324, 4, 1, to_bytes_u32_arr(tile_offset, big_endian)), // TILE_OFFSETS
        (325, 4, 1, to_bytes_u32_arr(tile_byte_count, big_endian)), // TILE_BYTE_COUNTS
    ];
    tags_data.sort_by_key(|t| t.0);

    buf.extend_from_slice(&to_bytes_u16(tags_data.len() as u16, big_endian));
    for (tag, typ, count, val) in &tags_data {
        buf.extend_from_slice(&to_bytes_u16(*tag, big_endian));
        buf.extend_from_slice(&to_bytes_u16(*typ, big_endian));
        buf.extend_from_slice(&to_bytes_u32(*count, big_endian));
        buf.extend_from_slice(val);
    }
    buf.extend_from_slice(&to_bytes_u32(0, big_endian)); // next IFD = 0

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

fn to_bytes_u16(v: u16, big_endian: bool) -> [u8; 2] {
    if big_endian {
        v.to_be_bytes()
    } else {
        v.to_le_bytes()
    }
}

fn to_bytes_u32(v: u32, big_endian: bool) -> [u8; 4] {
    if big_endian {
        v.to_be_bytes()
    } else {
        v.to_le_bytes()
    }
}

fn to_bytes_u32_arr(v: u32, big_endian: bool) -> [u8; 4] {
    to_bytes_u32(v, big_endian)
}

/// Encode a SHORT value in the 4-byte tag value slot.
fn to_short_in_long(v: u16, big_endian: bool) -> [u8; 4] {
    let mut arr = [0u8; 4];
    let bytes = to_bytes_u16(v, big_endian);
    arr[..2].copy_from_slice(&bytes);
    arr
}

/// Build a tiled TIFF with uncompressed u16 grayscale data to test endianness.
pub(super) fn build_u16_grayscale_tiff(width: u32, height: u32, big_endian: bool) -> NamedTempFile {
    let bom: &[u8] = if big_endian { b"MM" } else { b"II" };
    let spp: u32 = 1;
    let pixel_count = (width * height) as usize;

    // Write u16 test pattern: value = x + y * width
    let mut raw = Vec::with_capacity(pixel_count * 2);
    for y in 0..height {
        for x in 0..width {
            let val = (x + y * width) as u16;
            if big_endian {
                raw.extend_from_slice(&val.to_be_bytes());
            } else {
                raw.extend_from_slice(&val.to_le_bytes());
            }
        }
    }

    let mut buf = Vec::new();
    buf.extend_from_slice(bom);
    buf.extend_from_slice(&to_bytes_u16(42, big_endian));
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&to_bytes_u32(0, big_endian));

    let tile_offset = buf.len() as u32;
    let tile_byte_count = raw.len() as u32;
    buf.extend_from_slice(&raw);

    let ifd_offset = buf.len() as u32;
    {
        let p = first_ifd_pos;
        buf[p..p + 4].copy_from_slice(&to_bytes_u32(ifd_offset, big_endian));
    }

    let mut tags_data: Vec<(u16, u16, u32, [u8; 4])> = vec![
        (256, 4, 1, to_bytes_u32_arr(width, big_endian)),
        (257, 4, 1, to_bytes_u32_arr(height, big_endian)),
        (258, 3, 1, to_short_in_long(16, big_endian)), // BITS_PER_SAMPLE = 16
        (259, 3, 1, to_short_in_long(1, big_endian)),  // COMPRESSION = None
        (262, 3, 1, to_short_in_long(1, big_endian)),  // PHOTOMETRIC = MinIsBlack
        (277, 3, 1, to_short_in_long(spp as u16, big_endian)),
        (322, 4, 1, to_bytes_u32_arr(width, big_endian)),
        (323, 4, 1, to_bytes_u32_arr(height, big_endian)),
        (324, 4, 1, to_bytes_u32_arr(tile_offset, big_endian)),
        (325, 4, 1, to_bytes_u32_arr(tile_byte_count, big_endian)),
    ];
    tags_data.sort_by_key(|t| t.0);

    buf.extend_from_slice(&to_bytes_u16(tags_data.len() as u16, big_endian));
    for (tag, typ, count, val) in &tags_data {
        buf.extend_from_slice(&to_bytes_u16(*tag, big_endian));
        buf.extend_from_slice(&to_bytes_u16(*typ, big_endian));
        buf.extend_from_slice(&to_bytes_u32(*count, big_endian));
        buf.extend_from_slice(val);
    }
    buf.extend_from_slice(&to_bytes_u32(0, big_endian));

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

/// Build a MinIsWhite grayscale TIFF.
pub(super) fn build_min_is_white_tiff(width: u32, height: u32) -> NamedTempFile {
    let spp: u32 = 1;
    let raw_size = (width * height) as usize;
    // White background (0 = white in MinIsWhite), pattern: value = x
    let mut raw = vec![0u8; raw_size];
    for y in 0..height as usize {
        for x in 0..width as usize {
            raw[y * width as usize + x] = (x % 256) as u8;
        }
    }

    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    let first_ifd_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes());

    let tile_offset = buf.len() as u32;
    let tile_byte_count = raw.len() as u32;
    buf.extend_from_slice(&raw);

    let ifd_offset = buf.len() as u32;
    buf[first_ifd_pos..first_ifd_pos + 4].copy_from_slice(&ifd_offset.to_le_bytes());

    let mut tags_data: Vec<(u16, u16, u32, [u8; 4])> = vec![
        (256, 4, 1, width.to_le_bytes()),
        (257, 4, 1, height.to_le_bytes()),
        (258, 3, 1, {
            let mut v = [0u8; 4];
            v[..2].copy_from_slice(&8u16.to_le_bytes());
            v
        }),
        (259, 3, 1, {
            let mut v = [0u8; 4];
            v[..2].copy_from_slice(&1u16.to_le_bytes());
            v
        }), // None
        (262, 3, 1, {
            let mut v = [0u8; 4];
            v[..2].copy_from_slice(&0u16.to_le_bytes());
            v
        }), // MinIsWhite
        (277, 3, 1, {
            let mut v = [0u8; 4];
            v[..2].copy_from_slice(&(spp as u16).to_le_bytes());
            v
        }),
        (322, 4, 1, width.to_le_bytes()),
        (323, 4, 1, height.to_le_bytes()),
        (324, 4, 1, tile_offset.to_le_bytes()),
        (325, 4, 1, tile_byte_count.to_le_bytes()),
    ];
    tags_data.sort_by_key(|t| t.0);

    buf.extend_from_slice(&(tags_data.len() as u16).to_le_bytes());
    for (tag, typ, count, val) in &tags_data {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&typ.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(val);
    }
    buf.extend_from_slice(&0u32.to_le_bytes());

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}
