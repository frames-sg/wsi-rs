use super::*;

pub(super) fn read_record_bytes(record: &MiraxRecord) -> Result<Vec<u8>, WsiError> {
    read_record_bytes_fields(&record.path, record.offset, record.len)
}

pub(super) fn read_jpeg_dimensions_from_record(
    path: &Path,
    quickhash_files: &mut HashMap<PathBuf, File>,
    record: &MiraxRecord,
) -> Result<(u32, u32), WsiError> {
    let file = if let Some(file) = quickhash_files.get_mut(&record.path) {
        file
    } else {
        let file = File::open(&record.path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: record.path.clone(),
        })?;
        quickhash_files.insert(record.path.clone(), file);
        quickhash_files
            .get_mut(&record.path)
            .expect("MIRAX cached file must exist after insertion")
    };
    file.seek(SeekFrom::Start(record.offset))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: record.path.clone(),
        })?;

    let probe_len = record.len.min(MIRAX_ASSOCIATED_DIMENSION_PROBE_BYTES) as usize;
    let mut probe = vec![0u8; probe_len];
    file.read_exact(&mut probe)
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: record.path.clone(),
        })?;
    if let Ok(dimensions) = jpeg_dimensions(&probe) {
        return Ok(dimensions);
    }

    jpeg_dimensions(&read_record_bytes(record)?).map_err(|err| {
        invalid_slide(
            path,
            format!(
                "failed to derive MIRAX associated JPEG dimensions from {}: {err}",
                record.path.display()
            ),
        )
    })
}

pub(super) fn read_record_bytes_fields(
    path: &Path,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>, WsiError> {
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    read_record_bytes_from_file(&mut file, path, offset, len)
}

pub(super) fn read_record_bytes_from_file(
    file: &mut File,
    path: &Path,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>, WsiError> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf)
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    Ok(buf)
}

pub(super) fn quickhash_file_part_cached(
    quickhash: &mut Quickhash1,
    files: &mut HashMap<PathBuf, File>,
    path: &Path,
    offset: u64,
    len: u64,
) -> Result<(), WsiError> {
    let file = if let Some(file) = files.get_mut(path) {
        file
    } else {
        let file = File::open(path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
        files.insert(path.to_path_buf(), file);
        files
            .get_mut(path)
            .expect("MIRAX quickhash file must exist after insertion")
    };
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;

    let mut remaining = len;
    let mut buf = [0u8; MIRAX_QUICKHASH_READ_BUFFER_BYTES];
    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let read = file
            .read(&mut buf[..to_read])
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            })?;
        if read == 0 {
            break;
        }
        quickhash.update(&buf[..read]);
        remaining -= read as u64;
    }
    Ok(())
}

pub(super) fn rgb_image_to_sample_buffer(image: image::RgbImage) -> CpuTile {
    CpuTile::new(
        image.width(),
        image.height(),
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(image.into_raw()),
    )
    .expect("RGB image dimensions must match")
}

pub(super) fn parse_image_format(value: &str) -> Result<MiraxImageFormat, WsiError> {
    match value {
        "JPEG" => Ok(MiraxImageFormat::Jpeg),
        "PNG" => Ok(MiraxImageFormat::Png),
        "BMP24" => Ok(MiraxImageFormat::Bmp24),
        _ => Err(WsiError::DisplayConversion(format!(
            "unsupported MIRAX image format {value}"
        ))),
    }
}

pub(super) fn bgr_to_rgb(bgr: u32) -> u32 {
    ((bgr << 16) & 0x00FF0000) | (bgr & 0x0000FF00) | ((bgr >> 16) & 0x000000FF)
}

pub(super) fn irregular_extra_tiles(
    offset_x: f64,
    offset_y: f64,
    tile_advance_x: f64,
    tile_advance_y: f64,
    tile_width: f64,
    tile_height: f64,
) -> (u32, u32, u32, u32) {
    let extra_right = if offset_x < 0.0 {
        (-offset_x / tile_advance_x).ceil() as u32
    } else {
        0
    };
    let offset_xr = offset_x + (tile_width - tile_advance_x);
    let extra_left = if offset_xr > 0.0 {
        (offset_xr / tile_advance_x).ceil() as u32
    } else {
        0
    };
    let extra_bottom = if offset_y < 0.0 {
        (-offset_y / tile_advance_y).ceil() as u32
    } else {
        0
    };
    let offset_yr = offset_y + (tile_height - tile_advance_y);
    let extra_top = if offset_yr > 0.0 {
        (offset_yr / tile_advance_y).ceil() as u32
    } else {
        0
    };
    (extra_top, extra_bottom, extra_left, extra_right)
}

pub(super) fn required_ini_string(
    path: &Path,
    group: &HashMap<String, String>,
    key: &str,
) -> Result<String, WsiError> {
    group
        .get(key)
        .cloned()
        .ok_or_else(|| invalid_slide(path, format!("missing MIRAX key {key}")))
}

pub(super) fn parse_ini_i32(
    path: &Path,
    group: &HashMap<String, String>,
    key: &str,
) -> Result<i32, WsiError> {
    group
        .get(key)
        .ok_or_else(|| invalid_slide(path, format!("missing MIRAX key {key}")))?
        .parse::<i32>()
        .map_err(|_| invalid_slide(path, format!("invalid MIRAX integer for {key}")))
}

pub(super) fn parse_ini_u32(
    path: &Path,
    group: &HashMap<String, String>,
    key: &str,
) -> Result<u32, WsiError> {
    parse_u32_value(
        path,
        key,
        group
            .get(key)
            .ok_or_else(|| invalid_slide(path, format!("missing MIRAX key {key}")))?,
    )
}

pub(super) fn parse_u32_value(path: &Path, key: &str, value: &str) -> Result<u32, WsiError> {
    value
        .parse::<u32>()
        .map_err(|_| invalid_slide(path, format!("invalid MIRAX integer for {key}")))
}

pub(super) fn parse_ini_f64(
    path: &Path,
    group: &HashMap<String, String>,
    key: &str,
) -> Result<f64, WsiError> {
    group
        .get(key)
        .ok_or_else(|| invalid_slide(path, format!("missing MIRAX key {key}")))?
        .parse::<f64>()
        .map_err(|_| invalid_slide(path, format!("invalid MIRAX float for {key}")))
}

pub(super) fn read_exact_string(
    file: &mut File,
    path: &Path,
    len: usize,
) -> Result<String, WsiError> {
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub(super) fn read_i32_le(file: &mut File, path: &Path) -> Result<i32, WsiError> {
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf)
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    Ok(i32::from_le_bytes(buf))
}

pub(super) fn read_u32_le(file: &mut File, path: &Path) -> Result<u32, WsiError> {
    let value = read_i32_le(file, path)?;
    if value < 0 {
        return Err(invalid_slide(
            path,
            format!("negative MIRAX pointer value {value}"),
        ));
    }
    Ok(value as u32)
}

pub(super) fn fmt_key(fmt: &str, value: i32) -> String {
    fmt.replacen("%d", &value.to_string(), 1)
}

pub(super) fn fmt_key2(fmt: &str, value1: i32, value2: i32) -> String {
    fmt.replacen("%d", &value1.to_string(), 1)
        .replacen("%d", &value2.to_string(), 1)
}

pub(super) fn invalid_slide(path: &Path, message: impl Into<String>) -> WsiError {
    WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

pub(super) fn dataset_id_from_quickhash(
    path: &Path,
    quickhash: &str,
) -> Result<DatasetId, WsiError> {
    if quickhash.len() < 32 {
        return Err(invalid_slide(path, "quickhash too short"));
    }
    let value = u128::from_str_radix(&quickhash[..32], 16)
        .map_err(|_| invalid_slide(path, "quickhash is not valid hex"))?;
    Ok(DatasetId::new(value))
}
