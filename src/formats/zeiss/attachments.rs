use super::preflight::preflight_czi_file;
use super::tiles::bitmap_to_sample_buffer;
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

pub(super) fn decode_associated_attachment(
    czi: &mut CziFile,
    attachment: &czi_rs::AttachmentInfo,
) -> Result<Option<(AssociatedImage, CpuTile)>, WsiError> {
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
            },
            buffer,
        )));
    }

    if attachment.content_file_type.eq_ignore_ascii_case("CZI") {
        return with_temporary_czi_blob(&blob.data, |temp_path| {
            preflight_czi_file(temp_path)?;
            let mut embedded = CziFile::open(temp_path)
                .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
            ensure_uncompressed_embedded_czi(
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
            ensure_embedded_czi_plane_budget((plane_rect.w, plane_rect.h), max_bytes_per_pixel)?;
            let bitmap = embedded
                .read_frame_2d(0, 0, 0, 0)
                .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
            let buffer = bitmap_to_sample_buffer(bitmap)?;
            Ok::<_, WsiError>((
                AssociatedImage {
                    dimensions: (buffer.width, buffer.height),
                    sample_type: buffer.data.sample_type(),
                    channels: buffer.channels,
                },
                buffer,
            ))
        })
        .map(Some);
    }

    Ok(None)
}

fn ensure_uncompressed_embedded_czi(
    compressions: impl IntoIterator<Item = CziCompressionMode>,
) -> Result<(), WsiError> {
    if let Some(compression) = compressions
        .into_iter()
        .find(|compression| *compression != CziCompressionMode::UnCompressed)
    {
        return Err(WsiError::UnsupportedFormat(format!(
            "compressed embedded CZI associated images are not supported safely ({compression})"
        )));
    }
    Ok(())
}

fn ensure_embedded_czi_plane_budget(
    dimensions: (i32, i32),
    bytes_per_pixel: usize,
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
        MAX_DECODED_IMAGE_BYTES,
        "embedded CZI plane",
    )
    .map(|_| ())
    .map_err(WsiError::DisplayConversion)
}

pub(super) fn probe_associated_attachment(
    path: &Path,
    czi: &mut CziFile,
    attachment: &czi_rs::AttachmentInfo,
) -> Result<Option<AssociatedImage>, WsiError> {
    if attachment.content_file_type.eq_ignore_ascii_case("JPG") {
        if let Ok(bytes) = read_attachment_prefix(path, attachment, ASSOCIATED_JPEG_PROBE_BYTES) {
            if let Ok((width, height)) = crate::decode::jpeg::jpeg_dimensions(&bytes) {
                return Ok(Some(AssociatedImage {
                    dimensions: (width, height),
                    sample_type: SampleType::Uint8,
                    channels: 3,
                }));
            }
        }
    }

    Ok(decode_associated_attachment(czi, attachment)?.map(|(metadata, _buffer)| metadata))
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
