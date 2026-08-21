use super::*;

pub(super) fn is_supported_stripped_rgb_ifd(container: &TiffContainer, ifd_id: IfdId) -> bool {
    let Ok(width) = container.get_u64(ifd_id, tags::IMAGE_WIDTH) else {
        return false;
    };
    let Ok(height) = container.get_u64(ifd_id, tags::IMAGE_LENGTH) else {
        return false;
    };
    if width == 0 || height == 0 || width > u64::from(u32::MAX) || height > u64::from(u32::MAX) {
        return false;
    }
    let Some(decoded_bytes) = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
    else {
        return false;
    };
    if decoded_bytes > MAX_DECODED_IMAGE_BYTES {
        return false;
    }

    if container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1) != 1
        || container.get_u32(ifd_id, tags::PHOTOMETRIC).unwrap_or(0) != 2
        || container
            .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(1)
            != 3
        || container.get_u32(ifd_id, tags::ORIENTATION).unwrap_or(1) != 1
        || container.get_u32(ifd_id, tags::PREDICTOR).unwrap_or(1) != 1
    {
        return false;
    }
    let planar = container
        .get_u32(ifd_id, tags::PLANAR_CONFIGURATION)
        .unwrap_or(1);
    if !matches!(planar, 1 | 2) {
        return false;
    }
    if !container
        .get_u64_array(ifd_id, tags::BITS_PER_SAMPLE)
        .is_ok_and(|values| !values.is_empty() && values.iter().all(|&value| value == 8))
    {
        return false;
    }
    if container
        .get_u64_array(ifd_id, tags::SAMPLE_FORMAT)
        .is_ok_and(|values| values.iter().any(|&value| value != 1))
    {
        return false;
    }

    let rows_per_strip = u64::from(
        container
            .get_u32(ifd_id, tags::ROWS_PER_STRIP)
            .unwrap_or(height as u32),
    );
    if rows_per_strip == 0 {
        return false;
    }
    let strips_per_plane = height.div_ceil(rows_per_strip);
    let expected_strips = strips_per_plane * if planar == 2 { 3 } else { 1 };
    let Ok(strip_offsets) = container.get_u64_array(ifd_id, tags::STRIP_OFFSETS) else {
        return false;
    };
    let Ok(strip_byte_counts) = container.get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS) else {
        return false;
    };
    let total_strip_bytes = strip_byte_counts
        .iter()
        .try_fold(0u64, |total, &count| total.checked_add(count));
    strip_offsets.len() as u64 == expected_strips
        && strip_byte_counts.len() as u64 == expected_strips
        && strip_offsets.iter().all(|&offset| offset > 0)
        && strip_byte_counts.iter().all(|&count| count > 0)
        && total_strip_bytes == Some(decoded_bytes)
}
