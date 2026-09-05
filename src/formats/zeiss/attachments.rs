use super::preflight::{preflight_czi_file_with_limits, preflight_czi_subblock_with_limits};
use super::raster::{bitmap_to_sample_buffer, blit_tile};
use super::subblock::bitmap_from_raw_subblock;
use super::*;

const ASSOCIATED_JPEG_PROBE_BYTES: u64 = 256 << 10;

pub(super) fn associated_name(name: &str) -> Option<&'static str> {
    match name {
        "Label" => Some("label"),
        "SlidePreview" => Some("macro"),
        "Thumbnail" => Some("thumbnail"),
        _ => None,
    }
}

fn check_attachment_size(
    attachment: &czi_rs::AttachmentInfo,
    limits: crate::SlideLimits,
) -> Result<(), WsiError> {
    if attachment.data_size > limits.encoded_unit_bytes() {
        return Err(WsiError::ResourceLimit {
            resource: "CZI attachment",
            requested: attachment.data_size,
            limit: limits.encoded_unit_bytes(),
        });
    }
    Ok(())
}

pub(super) fn decode_associated_attachment(
    czi: &mut CziFile,
    attachment: &czi_rs::AttachmentInfo,
    limits: crate::SlideLimits,
) -> Result<Option<(AssociatedImage, CpuTile)>, WsiError> {
    check_attachment_size(attachment, limits)?;
    let blob: AttachmentBlob = czi
        .read_attachment(attachment.index)
        .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;

    if attachment.content_file_type.eq_ignore_ascii_case("JPG") {
        let buffer = crate::core::batch::exactly_one(
            decode_batch_jpeg(&[JpegDecodeJob {
                data: Cow::Borrowed(&blob.data),
                tables: None,
                expected_width: 0,
                expected_height: 0,
                color_transform: j2k_jpeg::ColorTransform::Auto,
                force_dimensions: false,
                requested_size: None,
            }]),
            "CZI associated JPEG decode",
        )??;
        return Ok(Some((
            AssociatedImage {
                dimensions: (buffer.width, buffer.height),
                sample_type: SampleType::Uint8,
                channels: 3,
                icc_profile: Vec::new(),
            },
            buffer,
        )));
    }

    if attachment.content_file_type.eq_ignore_ascii_case("CZI") {
        return with_temporary_czi_blob(&blob.data, |temp_path| {
            preflight_czi_file_with_limits(temp_path, limits)?;
            let mut embedded = CziFile::open(temp_path)
                .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
            ensure_supported_embedded_czi(
                embedded.subblocks().iter().map(|info| info.compression),
            )?;
            let plane_rect = embedded
                .statistics()
                .bounding_box_layer0
                .or(embedded.statistics().bounding_box)
                .ok_or_else(|| {
                    WsiError::DisplayConversion("embedded CZI has no plane bounding box".into())
                })?;
            let max_bytes_per_pixel = embedded
                .subblocks()
                .iter()
                .filter(|info| info.is_layer0())
                .map(|info| info.pixel_type.bytes_per_pixel())
                .max()
                .unwrap_or(1);
            ensure_embedded_czi_plane_budget_with_limit(
                (plane_rect.w, plane_rect.h),
                max_bytes_per_pixel,
                limits.decoded_output_bytes(),
            )?;
            let bitmap = read_embedded_czi_plane(&mut embedded, plane_rect, temp_path, limits)?;
            let buffer = bitmap_to_sample_buffer(bitmap)?;
            Ok::<_, WsiError>((
                AssociatedImage {
                    dimensions: (buffer.width, buffer.height),
                    sample_type: buffer.data.sample_type(),
                    channels: buffer.channels,
                    icc_profile: Vec::new(),
                },
                buffer,
            ))
        })
        .map(Some);
    }

    Ok(None)
}

fn ensure_supported_embedded_czi(
    compressions: impl IntoIterator<Item = CziCompressionMode>,
) -> Result<(), WsiError> {
    if let Some(compression) = compressions.into_iter().find(|compression| {
        !matches!(
            compression,
            CziCompressionMode::UnCompressed | CziCompressionMode::Jpg | CziCompressionMode::JpgXr
        )
    }) {
        return Err(WsiError::UnsupportedFormat(format!(
            "embedded CZI associated-image compression is not supported safely ({compression})"
        )));
    }
    Ok(())
}

fn read_embedded_czi_plane(
    embedded: &mut CziFile,
    plane_rect: IntRect,
    path: &Path,
    limits: crate::SlideLimits,
) -> Result<czi_rs::Bitmap, WsiError> {
    let statistics = embedded.statistics();
    let mut matching: Vec<_> = embedded
        .subblocks()
        .iter()
        .filter(|subblock| {
            subblock.is_layer0()
                && CziDimension::FRAME_ORDER.iter().all(|&dimension| {
                    let Some(interval) = statistics.dim_bounds.get(dimension) else {
                        return true;
                    };
                    match subblock.coordinate.get(dimension) {
                        Some(value) => value == interval.start,
                        None => interval.size <= 1,
                    }
                })
        })
        .cloned()
        .collect();
    if matching.is_empty() {
        return Err(WsiError::DisplayConversion(
            "embedded CZI has no layer-0 subblocks for its default plane".into(),
        ));
    }
    matching.sort_by_key(|subblock| (subblock.m_index.unwrap_or(i32::MIN), subblock.file_position));

    let pixel_type = matching[0].pixel_type;
    if matching
        .iter()
        .any(|subblock| subblock.pixel_type != pixel_type)
    {
        return Err(WsiError::DisplayConversion(
            "embedded CZI default plane contains mixed pixel types".into(),
        ));
    }
    let mut bitmap = czi_rs::Bitmap::zeros(
        pixel_type,
        u32::try_from(plane_rect.w).map_err(|_| {
            WsiError::DisplayConversion("embedded CZI plane width is invalid".into())
        })?,
        u32::try_from(plane_rect.h).map_err(|_| {
            WsiError::DisplayConversion("embedded CZI plane height is invalid".into())
        })?,
    )
    .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
    for info in matching {
        preflight_czi_subblock_with_limits(path, info.file_position, limits)?;
        let raw = embedded
            .read_subblock(info.index)
            .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
        let tile = bitmap_from_raw_subblock(&raw, limits)?;
        blit_tile(
            &mut bitmap,
            &tile,
            info.rect.x - plane_rect.x,
            info.rect.y - plane_rect.y,
        )?;
    }
    Ok(bitmap)
}

#[cfg(test)]
fn ensure_embedded_czi_plane_budget(
    dimensions: (i32, i32),
    bytes_per_pixel: usize,
) -> Result<(), WsiError> {
    ensure_embedded_czi_plane_budget_with_limit(
        dimensions,
        bytes_per_pixel,
        MAX_DECODED_IMAGE_BYTES,
    )
}

fn ensure_embedded_czi_plane_budget_with_limit(
    dimensions: (i32, i32),
    bytes_per_pixel: usize,
    limit: u64,
) -> Result<(), WsiError> {
    let width = u64::try_from(dimensions.0).map_err(|_| {
        WsiError::DisplayConversion("embedded CZI plane has a non-positive width".into())
    })?;
    let height = u64::try_from(dimensions.1).map_err(|_| {
        WsiError::DisplayConversion("embedded CZI plane has a non-positive height".into())
    })?;
    if width == 0 || height == 0 {
        return Err(WsiError::DisplayConversion(
            "embedded CZI plane has zero dimensions".into(),
        ));
    }
    checked_product_to_usize(
        &[
            width,
            height,
            u64::try_from(bytes_per_pixel).unwrap_or(u64::MAX),
        ],
        limit.min(MAX_DECODED_IMAGE_BYTES),
        "embedded CZI plane",
    )
    .map(|_| ())
    .map_err(WsiError::DisplayConversion)
}

pub(super) fn probe_associated_attachment(
    path: &Path,
    czi: &mut CziFile,
    attachment: &czi_rs::AttachmentInfo,
    limits: crate::SlideLimits,
) -> Result<Option<AssociatedImage>, WsiError> {
    check_attachment_size(attachment, limits)?;
    if attachment.content_file_type.eq_ignore_ascii_case("JPG") {
        if let Ok(bytes) = read_attachment_prefix(path, attachment, ASSOCIATED_JPEG_PROBE_BYTES) {
            if let Ok((width, height)) = crate::decode::jpeg::jpeg_dimensions(&bytes) {
                return Ok(Some(AssociatedImage {
                    dimensions: (width, height),
                    sample_type: SampleType::Uint8,
                    channels: 3,
                    icc_profile: Vec::new(),
                }));
            }
        }
    }

    Ok(decode_associated_attachment(czi, attachment, limits)?.map(|(metadata, _buffer)| metadata))
}

fn read_attachment_prefix(
    path: &Path,
    attachment: &czi_rs::AttachmentInfo,
    max_bytes: u64,
) -> Result<Vec<u8>, WsiError> {
    let payload_offset = attachment
        .file_position
        .checked_add(32 + 256)
        .ok_or_else(|| WsiError::DisplayConversion("Zeiss attachment offset overflow".into()))?;
    let read_len = attachment.data_size.min(max_bytes);
    let read_len_usize = usize::try_from(read_len).map_err(|_| {
        WsiError::DisplayConversion("Zeiss attachment probe length overflow".into())
    })?;
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    file.seek(SeekFrom::Start(payload_offset))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let mut buffer = vec![0u8; read_len_usize];
    file.read_exact(&mut buffer)
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    Ok(buffer)
}

fn with_temporary_czi_blob<T>(
    data: &[u8],
    operation: impl FnOnce(&Path) -> Result<T, WsiError>,
) -> Result<T, WsiError> {
    with_temporary_czi_blob_in(&std::env::temp_dir(), data, operation)
}

fn with_temporary_czi_blob_in<T>(
    directory: &Path,
    data: &[u8],
    operation: impl FnOnce(&Path) -> Result<T, WsiError>,
) -> Result<T, WsiError> {
    let mut temporary = tempfile::Builder::new()
        .prefix("wsi-rs-zeiss-")
        .suffix(".czi")
        .tempfile_in(directory)
        .map_err(WsiError::Io)?;
    let path = temporary.path().to_path_buf();
    temporary
        .as_file_mut()
        .write_all(data)
        .and_then(|()| temporary.as_file_mut().flush())
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path,
        })?;
    operation(temporary.path())
}

pub(super) fn guid_bytes(value: &str) -> Result<[u8; 16], WsiError> {
    let parts: Vec<_> = value.split('-').collect();
    if parts.len() != 5
        || parts[0].len() != 8
        || parts[1].len() != 4
        || parts[2].len() != 4
        || parts[3].len() != 4
        || parts[4].len() != 12
    {
        return Err(WsiError::DisplayConversion(format!(
            "unexpected Zeiss GUID format: {value}"
        )));
    }

    fn parse_hex_pair(value: &str, start: usize) -> Result<u8, WsiError> {
        u8::from_str_radix(&value[start..start + 2], 16)
            .map_err(|_| WsiError::DisplayConversion(format!("invalid GUID hex: {value}")))
    }

    let mut bytes = [0u8; 16];

    // CZI stores GUIDs with the first three fields little-endian in-file, and
    // Compatibility hashing uses those raw bytes directly.
    for (idx, start) in [6, 4, 2, 0].into_iter().enumerate() {
        bytes[idx] = parse_hex_pair(parts[0], start)?;
    }
    for (idx, start) in [2, 0].into_iter().enumerate() {
        bytes[4 + idx] = parse_hex_pair(parts[1], start)?;
        bytes[6 + idx] = parse_hex_pair(parts[2], start)?;
    }
    for (idx, start) in [0, 2].into_iter().enumerate() {
        bytes[8 + idx] = parse_hex_pair(parts[3], start)?;
    }
    for idx in 0..6 {
        bytes[10 + idx] = parse_hex_pair(parts[4], idx * 2)?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
