use super::tiles::bitmap_to_sample_buffer;
use super::*;

const ASSOCIATED_JPEG_PROBE_BYTES: u64 = 256 << 10;
static TEMP_BLOB_COUNTER: AtomicU64 = AtomicU64::new(0);

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
        let buffer = decode_batch_jpeg(&[JpegDecodeJob {
            data: Cow::Borrowed(&blob.data),
            tables: None,
            expected_width: 0,
            expected_height: 0,
            color_transform: signinum_jpeg::ColorTransform::Auto,
            force_dimensions: false,
            requested_size: None,
        }])
        .into_iter()
        .next()
        .expect("1-element JPEG facade batch")?;
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
        let temp_path = temp_czi_path(attachment.index);
        fs::write(&temp_path, &blob.data).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: temp_path.clone(),
        })?;
        let result = (|| {
            let mut embedded = CziFile::open(&temp_path)
                .map_err(|source| WsiError::DisplayConversion(source.to_string()))?;
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
        })();
        let _ = fs::remove_file(&temp_path);
        return result.map(Some);
    }

    Ok(None)
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

fn temp_czi_path(index: usize) -> PathBuf {
    let counter = TEMP_BLOB_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "statumen-zeiss-{}-{}-{}.czi",
        std::process::id(),
        index,
        counter
    ))
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
