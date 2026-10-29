use super::*;

/// TIFF Compression tag values the stripped level reader decodes. Old-style
/// JPEG (6) is excluded: its strips are not self-contained JPEG streams.
const COMPRESSION_NONE: u32 = 1;
const COMPRESSION_LZW: u32 = 5;
const COMPRESSION_JPEG: u32 = 7;
const COMPRESSION_DEFLATE: u32 = 8;
const COMPRESSION_ADOBE_DEFLATE: u32 = 32946;
const COMPRESSION_ZSTD: u32 = 50000;

/// Largest side of one JPEG frame; each JPEG strip is one frame.
const MAX_JPEG_FRAME_SIDE: u64 = u16::MAX as u64;

/// The top-level image a strip-only TIFF opens as: the largest by area, the
/// first in file order among equal sizes. Images without dimensions are
/// skipped.
pub(super) fn primary_stripped_ifd(container: &TiffContainer) -> Option<IfdId> {
    container
        .top_ifds()
        .iter()
        .filter_map(|&ifd_id| {
            let width = container.get_u64(ifd_id, tags::IMAGE_WIDTH).ok()?;
            let height = container.get_u64(ifd_id, tags::IMAGE_LENGTH).ok()?;
            (width > 0 && height > 0).then_some((ifd_id, level_area(width, height)))
        })
        .min_by_key(|&(_, area)| Reverse(area))
        .map(|(ifd_id, _)| ifd_id)
}

/// Rows in each strip of a `height`-row image: RowsPerStrip clamped to the
/// height, or the whole image when the tag is absent (the TIFF default).
/// `None` for a zero or unreadable value.
pub(super) fn rows_per_strip(container: &TiffContainer, ifd_id: IfdId, height: u64) -> Option<u64> {
    match container.get_u64(ifd_id, tags::ROWS_PER_STRIP) {
        Ok(0) => None,
        Ok(rows) => Some(rows.min(height)),
        Err(TiffParseError::TagNotFound { .. }) => Some(height),
        Err(_) => None,
    }
}

/// Whether the stripped level reader can decode `ifd_id`: an 8-bit grayscale
/// or RGB image at top-left orientation, stored uncompressed or with LZW,
/// Deflate, Zstd or JPEG, with one strip-table entry per strip. YCbCr is
/// accepted only inside JPEG, and separate sample planes only uncompressed.
///
/// Compressed strips are decoded whole, so each one must fit the
/// decoded-image limit; the image as a whole need not.
pub(super) fn is_supported_stripped_ifd(container: &TiffContainer, ifd_id: IfdId) -> bool {
    let Ok(width) = container.get_u64(ifd_id, tags::IMAGE_WIDTH) else {
        return false;
    };
    let Ok(height) = container.get_u64(ifd_id, tags::IMAGE_LENGTH) else {
        return false;
    };
    if width == 0 || height == 0 || width > u64::from(u32::MAX) || height > u64::from(u32::MAX) {
        return false;
    }

    let compression = container
        .get_u32(ifd_id, tags::COMPRESSION)
        .unwrap_or(COMPRESSION_NONE);
    let compressed = matches!(
        compression,
        COMPRESSION_LZW | COMPRESSION_DEFLATE | COMPRESSION_ADOBE_DEFLATE | COMPRESSION_ZSTD
    );
    let jpeg = compression == COMPRESSION_JPEG;
    if compression != COMPRESSION_NONE && !compressed && !jpeg {
        return false;
    }

    let samples = container
        .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
        .unwrap_or(1);
    let photometric = container
        .get_u32(ifd_id, tags::PHOTOMETRIC)
        .unwrap_or(if samples == 1 { 1 } else { 2 });
    // MinIsWhite is inverted by the raw-sample decoder, which JPEG bypasses.
    let color_supported = match (samples, photometric) {
        (1, 0) => !jpeg,
        (1, 1) | (3, 2) => true,
        (3, 6) => jpeg,
        _ => false,
    };
    if !color_supported
        || !container
            .get_u64_array(ifd_id, tags::BITS_PER_SAMPLE)
            .is_ok_and(|values| !values.is_empty() && values.iter().all(|&value| value == 8))
        || container
            .get_u64_array(ifd_id, tags::SAMPLE_FORMAT)
            .is_ok_and(|values| values.iter().any(|&value| value != 1))
        || container.get_u32(ifd_id, tags::ORIENTATION).unwrap_or(1) != 1
    {
        return false;
    }

    let predictor = container.get_u32(ifd_id, tags::PREDICTOR).unwrap_or(1);
    if predictor != 1 && !(predictor == 2 && compressed) {
        return false;
    }

    // One sample plane needs no planar handling whatever the tag says.
    let planar = if samples == 1 {
        1
    } else {
        container
            .get_u32(ifd_id, tags::PLANAR_CONFIGURATION)
            .unwrap_or(1)
    };
    let planes = match planar {
        1 => 1,
        2 if compression == COMPRESSION_NONE => 3,
        _ => return false,
    };

    let Some(rows_per_strip) = rows_per_strip(container, ifd_id, height) else {
        return false;
    };
    if jpeg && (width > MAX_JPEG_FRAME_SIDE || rows_per_strip > MAX_JPEG_FRAME_SIDE) {
        return false;
    }
    if compression != COMPRESSION_NONE {
        // JPEG strips decode to RGB, grayscale included.
        let decoded_samples = if jpeg { 3 } else { u64::from(samples) };
        let strip_bytes = width
            .checked_mul(rows_per_strip)
            .and_then(|pixels| pixels.checked_mul(decoded_samples));
        if !strip_bytes.is_some_and(|bytes| bytes <= MAX_DECODED_IMAGE_BYTES) {
            return false;
        }
    }

    let expected_strips = height.div_ceil(rows_per_strip) * planes;
    let Ok(strip_offsets) = container.get_u64_array(ifd_id, tags::STRIP_OFFSETS) else {
        return false;
    };
    let Ok(strip_byte_counts) = container.get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS) else {
        return false;
    };
    strip_offsets.len() as u64 == expected_strips
        && strip_byte_counts.len() as u64 == expected_strips
        && strip_offsets.iter().all(|&offset| offset > 0)
        && strip_byte_counts.iter().all(|&count| count > 0)
}
