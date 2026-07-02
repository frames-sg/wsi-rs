use super::helpers::*;
use super::*;
use crate::formats::geometry::irregular_extra_tiles;
use crate::formats::ini::parse_ini_file;

pub(super) fn parse_mirax_ini(path: &Path) -> Result<ParsedIni, WsiError> {
    parse_ini_file(
        path,
        SLIDEDAT_MAX_SIZE.max(KEY_FILE_MAX_SIZE),
        |path| invalid_slide(path, "MIRAX key file too large"),
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn process_hier_data_pages_from_indexfile(
    path: &Path,
    index_file: &mut File,
    index_path: &Path,
    mut seek_location: u64,
    datafile_paths: &[PathBuf],
    images_x: u32,
    images_y: u32,
    image_divisions: u32,
    params: &[SlideZoomLevelParams],
    levels: &mut [MiraxLevelBuilder],
    slide_positions: &[i32],
    quickhash: &mut Quickhash1,
    quickhash_files: &mut HashMap<PathBuf, File>,
) -> Result<(), WsiError> {
    let mut image_number = 0u32;
    let positions_x = images_x / image_divisions;
    let positions_y = images_y / image_divisions;
    let mut active_positions = vec![false; positions_x.saturating_mul(positions_y) as usize];
    let levels_len = levels.len();
    let level0_raw_image_width = levels[0].raw_image_width;
    let level0_raw_image_height = levels[0].raw_image_height;

    for zoom_level in 0..levels.len() {
        let level = &mut levels[zoom_level];
        let params_level = params[zoom_level];

        index_file
            .seek(SeekFrom::Start(seek_location))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: index_path.to_path_buf(),
            })?;
        let ptr = read_u32_le(index_file, index_path)? as u64;
        index_file
            .seek(SeekFrom::Start(ptr))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: index_path.to_path_buf(),
            })?;
        if read_u32_le(index_file, index_path)? != 0 {
            return Err(invalid_slide(
                path,
                format!("expected initial zero for MIRAX zoom level {zoom_level}"),
            ));
        }
        let initial_data_page = read_u32_le(index_file, index_path)? as u64;
        index_file
            .seek(SeekFrom::Start(initial_data_page))
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: index_path.to_path_buf(),
            })?;

        loop {
            let page_len = read_i32_le(index_file, index_path)?;
            if page_len < 0 {
                return Err(invalid_slide(path, "negative MIRAX page length"));
            }
            let next_ptr = read_i32_le(index_file, index_path)?;
            for _ in 0..page_len {
                let image_index = read_i32_le(index_file, index_path)?;
                let offset = read_i32_le(index_file, index_path)?;
                let length = read_i32_le(index_file, index_path)?;
                let fileno = read_i32_le(index_file, index_path)?;
                if image_index < 0 || offset < 0 || length < 0 || fileno < 0 {
                    return Err(invalid_slide(path, "negative MIRAX hier record component"));
                }
                let fileno = fileno as usize;
                let x = image_index as u32 % images_x;
                let y = image_index as u32 / images_x;
                if y >= images_y {
                    return Err(invalid_slide(
                        path,
                        format!("MIRAX image row {y} outside zoom level {zoom_level}"),
                    ));
                }
                if !x.is_multiple_of(params_level.image_concat)
                    || !y.is_multiple_of(params_level.image_concat)
                {
                    return Err(invalid_slide(
                        path,
                        format!("MIRAX image coordinates ({x},{y}) not aligned for zoom level {zoom_level}"),
                    ));
                }
                let datafile_path = datafile_paths
                    .get(fileno)
                    .ok_or_else(|| {
                        invalid_slide(path, format!("invalid MIRAX data file {fileno}"))
                    })?
                    .clone();
                if zoom_level == levels_len - 1 {
                    quickhash_file_part_cached(
                        quickhash,
                        quickhash_files,
                        &datafile_path,
                        offset as u64,
                        length as u64,
                    )?;
                }

                let image = Arc::new(MiraxImage {
                    id: image_number,
                    record: MiraxRecord {
                        path: datafile_path,
                        offset: offset as u64,
                        len: length as u64,
                    },
                    format: level.image_format,
                    expected_width: level.raw_image_width,
                    expected_height: level.raw_image_height,
                });
                image_number += 1;

                for yi in 0..params_level.tiles_per_image {
                    let yy = y + yi * image_divisions;
                    if yy >= images_y {
                        break;
                    }
                    for xi in 0..params_level.tiles_per_image {
                        let xx = x + xi * image_divisions;
                        if xx >= images_x {
                            break;
                        }
                        let Some((pos0_x, pos0_y)) = get_tile_position(
                            slide_positions,
                            &mut active_positions,
                            params,
                            images_x,
                            image_divisions,
                            level0_raw_image_width,
                            level0_raw_image_height,
                            zoom_level,
                            xx,
                            yy,
                        )?
                        else {
                            continue;
                        };
                        let pos_x = f64::from(pos0_x) / f64::from(params_level.image_concat);
                        let pos_y = f64::from(pos0_y) / f64::from(params_level.image_concat);
                        let src_x = (level.tile_width * f64::from(xi)).round() as u32;
                        let src_y = (level.tile_height * f64::from(yi)).round() as u32;
                        let tile_x = x / params_level.tile_count_divisor + xi;
                        let tile_y = y / params_level.tile_count_divisor + yi;
                        insert_tile(
                            level,
                            &params_level,
                            image.clone(),
                            pos_x,
                            pos_y,
                            src_x,
                            src_y,
                            tile_x as i64,
                            tile_y as i64,
                        );
                    }
                }
            }
            if next_ptr == 0 {
                break;
            }
            index_file
                .seek(SeekFrom::Start(next_ptr as u64))
                .map_err(|source| WsiError::IoWithPath {
                    source: Arc::new(source),
                    path: index_path.to_path_buf(),
                })?;
        }

        seek_location += 4;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn insert_tile(
    level: &mut MiraxLevelBuilder,
    params: &SlideZoomLevelParams,
    image: Arc<MiraxImage>,
    pos_x: f64,
    pos_y: f64,
    src_x: u32,
    src_y: u32,
    tile_x: i64,
    tile_y: i64,
) {
    let offset_x = pos_x - tile_x as f64 * params.tile_advance_x;
    let offset_y = pos_y - tile_y as f64 * params.tile_advance_y;
    let width = level.tile_width.ceil() as u32;
    let height = level.tile_height.ceil() as u32;
    let descriptor_index = level.descriptors.len();
    level.descriptors.push(MiraxTile {
        image,
        src_x,
        src_y,
    });
    level.tiles.insert(
        (tile_x, tile_y),
        TileEntry {
            offset: (offset_x, offset_y),
            dimensions: (width, height),
            tiff_tile_index: Some(descriptor_index),
        },
    );
    let extras = irregular_extra_tiles(
        offset_x,
        offset_y,
        params.tile_advance_x,
        params.tile_advance_y,
        level.tile_width,
        level.tile_height,
    );
    level.extra_tiles.0 = level.extra_tiles.0.max(extras.0);
    level.extra_tiles.1 = level.extra_tiles.1.max(extras.1);
    level.extra_tiles.2 = level.extra_tiles.2.max(extras.2);
    level.extra_tiles.3 = level.extra_tiles.3.max(extras.3);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn load_slide_positions(
    path: &Path,
    index_file: &mut File,
    datafile_paths: &[PathBuf],
    slide_id_len: usize,
    vimslide_record: Option<i32>,
    stitching_record: Option<i32>,
    images_x: u32,
    images_y: u32,
    image_divisions: u32,
    level0_image_concat: u32,
    image0_w: u32,
    image0_h: u32,
    overlap_x: f64,
    overlap_y: f64,
) -> Result<Vec<i32>, WsiError> {
    let positions_x = images_x / image_divisions;
    let positions_y = images_y / image_divisions;
    let npositions = positions_x.saturating_mul(positions_y) as usize;
    let expected_size = npositions * SLIDE_POSITION_RECORD_SIZE;
    let nonhier_root = (INDEX_VERSION.len() + slide_id_len + 4) as u64;

    if let Some(record) = vimslide_record.or(stitching_record) {
        let record = read_nonhier_record(path, index_file, datafile_paths, nonhier_root, record)?;
        let mut buffer = read_record_bytes_fields(&record.path, record.offset, record.len)?;
        if stitching_record == Some(record.index) {
            let mut decoder = ZlibDecoder::new(buffer.as_slice());
            let mut inflated = Vec::with_capacity(expected_size);
            decoder.read_to_end(&mut inflated).map_err(|err| {
                invalid_slide(
                    path,
                    format!("failed to inflate MIRAX position buffer: {err}"),
                )
            })?;
            buffer = inflated;
        }
        if buffer.len() != expected_size {
            return Err(invalid_slide(
                path,
                format!(
                    "unexpected MIRAX position buffer size {} (expected {expected_size})",
                    buffer.len()
                ),
            ));
        }
        return read_slide_position_buffer(path, &buffer, level0_image_concat);
    }

    let mut positions = Vec::with_capacity(npositions * 2);
    for i in 0..npositions {
        positions.push(
            ((i % positions_x as usize) as f64
                * (f64::from(image0_w) * f64::from(image_divisions) - overlap_x))
                as i32,
        );
        positions.push(
            ((i / positions_x as usize) as f64
                * (f64::from(image0_h) * f64::from(image_divisions) - overlap_y))
                as i32,
        );
    }
    Ok(positions)
}

pub(super) fn read_slide_position_buffer(
    path: &Path,
    buffer: &[u8],
    level0_image_concat: u32,
) -> Result<Vec<i32>, WsiError> {
    if !buffer.len().is_multiple_of(SLIDE_POSITION_RECORD_SIZE) {
        return Err(invalid_slide(path, "unexpected MIRAX position buffer size"));
    }
    let mut positions = Vec::with_capacity(buffer.len() / SLIDE_POSITION_RECORD_SIZE * 2);
    let mut cursor = 0usize;
    while cursor < buffer.len() {
        let flag = buffer[cursor];
        if flag & 0xfe != 0 {
            return Err(invalid_slide(
                path,
                format!("unexpected MIRAX position flag {flag}"),
            ));
        }
        cursor += 1;
        let x = i32::from_le_bytes(buffer[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        let y = i32::from_le_bytes(buffer[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        positions.push(x.saturating_mul(level0_image_concat as i32));
        positions.push(y.saturating_mul(level0_image_concat as i32));
    }
    Ok(positions)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn get_tile_position(
    slide_positions: &[i32],
    active_positions: &mut [bool],
    params: &[SlideZoomLevelParams],
    images_across: u32,
    image_divisions: u32,
    level0_image_width: u32,
    level0_image_height: u32,
    zoom_level: usize,
    xx: u32,
    yy: u32,
) -> Result<Option<(i32, i32)>, WsiError> {
    let params_level = params[zoom_level];
    let image0_w = level0_image_width as i32;
    let image0_h = level0_image_height as i32;
    let xp = xx / image_divisions;
    let yp = yy / image_divisions;
    let cp = (yp * (images_across / image_divisions) + xp) as usize;
    let Some(base_x) = slide_positions.get(cp * 2).copied() else {
        return Ok(None);
    };
    let Some(base_y) = slide_positions.get(cp * 2 + 1).copied() else {
        return Ok(None);
    };
    let pos0_x = base_x + image0_w * (xx as i32 - xp as i32 * image_divisions as i32);
    let pos0_y = base_y + image0_h * (yy as i32 - yp as i32 * image_divisions as i32);

    if zoom_level == 0 {
        if base_x == 0 && base_y == 0 && (xp != 0 || yp != 0) {
            return Ok(None);
        }
        active_positions[cp] = true;
        return Ok(Some((pos0_x, pos0_y)));
    }

    for ypp in yp..yp + params_level.positions_per_tile {
        for xpp in xp..xp + params_level.positions_per_tile {
            let cpp = (ypp * (images_across / image_divisions) + xpp) as usize;
            if active_positions.get(cpp).copied().unwrap_or(false) {
                return Ok(Some((pos0_x, pos0_y)));
            }
        }
    }
    Ok(None)
}

pub(super) fn build_associated_records(
    path: &Path,
    index_file: &mut File,
    datafile_paths: &[PathBuf],
    slide_id_len: usize,
    macro_record: Option<i32>,
    label_record: Option<i32>,
    thumbnail_record: Option<i32>,
) -> Result<HashMap<String, MiraxRecord>, WsiError> {
    let nonhier_root = (INDEX_VERSION.len() + slide_id_len + 4) as u64;
    let mut associated = HashMap::new();
    for (name, recordno) in [
        ("macro", macro_record),
        ("label", label_record),
        ("thumbnail", thumbnail_record),
    ] {
        let Some(recordno) = recordno else {
            continue;
        };
        let record = read_nonhier_record(path, index_file, datafile_paths, nonhier_root, recordno)?;
        associated.insert(
            name.into(),
            MiraxRecord {
                path: record.path,
                offset: record.offset,
                len: record.len,
            },
        );
    }
    Ok(associated)
}

struct MiraxNonHierRecord {
    index: i32,
    path: PathBuf,
    offset: u64,
    len: u64,
}

fn read_nonhier_record(
    path: &Path,
    index_file: &mut File,
    datafile_paths: &[PathBuf],
    nonhier_root: u64,
    record_index: i32,
) -> Result<MiraxNonHierRecord, WsiError> {
    if record_index < 0 {
        return Err(invalid_slide(path, "negative MIRAX nonhier record"));
    }
    index_file
        .seek(SeekFrom::Start(nonhier_root))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let table_base = read_u32_le(index_file, path)? as u64;
    index_file
        .seek(SeekFrom::Start(table_base + 4 * record_index as u64))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let list_head = read_u32_le(index_file, path)? as u64;
    index_file
        .seek(SeekFrom::Start(list_head))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    if read_u32_le(index_file, path)? != 0 {
        return Err(invalid_slide(
            path,
            "expected zero at beginning of MIRAX data page",
        ));
    }
    let page_ptr = read_u32_le(index_file, path)? as u64;
    index_file
        .seek(SeekFrom::Start(page_ptr))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let page_len = read_i32_le(index_file, path)?;
    if page_len < 1 {
        return Err(invalid_slide(path, "expected at least one MIRAX data item"));
    }
    let _next = read_i32_le(index_file, path)?;
    if read_i32_le(index_file, path)? != 0 || read_i32_le(index_file, path)? != 0 {
        return Err(invalid_slide(path, "unexpected MIRAX data page header"));
    }
    let offset = read_i32_le(index_file, path)?;
    let len = read_i32_le(index_file, path)?;
    let fileno = read_i32_le(index_file, path)?;
    if offset < 0 || len < 0 || fileno < 0 {
        return Err(invalid_slide(path, "negative MIRAX nonhier record payload"));
    }
    let datafile = datafile_paths
        .get(fileno as usize)
        .ok_or_else(|| invalid_slide(path, format!("invalid MIRAX data file {fileno}")))?;
    Ok(MiraxNonHierRecord {
        index: record_index,
        path: datafile.clone(),
        offset: offset as u64,
        len: len as u64,
    })
}

pub(super) fn verify_index_header(
    path: &Path,
    index_file: &mut File,
    slide_id: &str,
) -> Result<(), WsiError> {
    index_file
        .seek(SeekFrom::Start(0))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let version = read_exact_string(index_file, path, INDEX_VERSION.len())?;
    if version != INDEX_VERSION {
        return Err(invalid_slide(
            path,
            "Index.dat does not have expected version",
        ));
    }
    let found_uuid = read_exact_string(index_file, path, slide_id.len())?;
    if found_uuid != slide_id {
        return Err(invalid_slide(
            path,
            "Index.dat does not have matching slide identifier",
        ));
    }
    Ok(())
}

pub(super) fn get_associated_image_nonhier_offset(
    path: &Path,
    ini: &ParsedIni,
    nonhier_count: i32,
    group: &str,
    target_name: &str,
    target_value: &str,
    target_format_key: &str,
) -> Result<Option<i32>, WsiError> {
    let Some((offset, section_name)) =
        get_nonhier_val_offset(path, ini, nonhier_count, group, target_name, target_value)?
    else {
        return Ok(None);
    };
    let group = ini
        .groups
        .get(&section_name)
        .ok_or_else(|| invalid_slide(path, format!("missing MIRAX section {section_name}")))?;
    let format = required_ini_string(path, group, target_format_key)?;
    if parse_image_format(format.as_str())? != MiraxImageFormat::Jpeg {
        return Err(invalid_slide(
            path,
            format!("unsupported MIRAX associated image format {format}"),
        ));
    }
    Ok(Some(offset))
}

pub(super) fn get_nonhier_name_offset(
    path: &Path,
    ini: &ParsedIni,
    nonhier_count: i32,
    group: &str,
    target_name: &str,
) -> Result<Option<i32>, WsiError> {
    Ok(
        get_nonhier_name_offset_helper(path, ini, nonhier_count, group, target_name)?
            .map(|(offset, _, _)| offset),
    )
}

pub(super) fn get_nonhier_val_offset(
    path: &Path,
    ini: &ParsedIni,
    nonhier_count: i32,
    group: &str,
    target_name: &str,
    target_value: &str,
) -> Result<Option<(i32, String)>, WsiError> {
    let Some((mut offset, name_count, name_index)) =
        get_nonhier_name_offset_helper(path, ini, nonhier_count, group, target_name)?
    else {
        return Ok(None);
    };
    let group_map = ini
        .groups
        .get(group)
        .ok_or_else(|| invalid_slide(path, format!("missing MIRAX group {group}")))?;
    for i in 0..name_count {
        let value = required_ini_string(
            path,
            group_map,
            &fmt_key2(KEY_NONHIER_VAL_FMT, name_index, i),
        )?;
        if value == target_value {
            let section = required_ini_string(
                path,
                group_map,
                &fmt_key2(KEY_NONHIER_VAL_SECTION_FMT, name_index, i),
            )?;
            return Ok(Some((offset, section)));
        }
        offset += 1;
    }
    Ok(None)
}

pub(super) fn get_nonhier_name_offset_helper(
    path: &Path,
    ini: &ParsedIni,
    nonhier_count: i32,
    group: &str,
    target_name: &str,
) -> Result<Option<(i32, i32, i32)>, WsiError> {
    let group_map = ini
        .groups
        .get(group)
        .ok_or_else(|| invalid_slide(path, format!("missing MIRAX group {group}")))?;
    let mut offset = 0;
    for i in 0..nonhier_count {
        let name = required_ini_string(path, group_map, &fmt_key(KEY_NONHIER_NAME, i))?;
        let count = parse_ini_i32(path, group_map, &fmt_key(KEY_NONHIER_COUNT_FMT, i))?;
        if count <= 0 {
            return Err(invalid_slide(path, "MIRAX nonhier count is zero"));
        }
        if name == target_name {
            return Ok(Some((offset, count, i)));
        }
        offset += count;
    }
    Ok(None)
}
