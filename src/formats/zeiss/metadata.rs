use super::attachments::guid_bytes;
use super::*;

const DEFAULT_TILE_PX: u32 = 256;

pub(super) fn build_channels(summary: &czi_rs::MetadataSummary) -> Vec<ChannelInfo> {
    summary
        .channels
        .first()
        .map(|channel| ChannelInfo {
            name: channel.name.clone(),
            color: channel.color.as_deref().and_then(parse_channel_color),
            excitation_nm: None,
            emission_nm: None,
        })
        .into_iter()
        .collect()
}

fn parse_channel_color(value: &str) -> Option<[u8; 3]> {
    let trimmed = value.trim().trim_start_matches('#');
    if trimmed.len() == 8 {
        let r = u8::from_str_radix(&trimmed[2..4], 16).ok()?;
        let g = u8::from_str_radix(&trimmed[4..6], 16).ok()?;
        let b = u8::from_str_radix(&trimmed[6..8], 16).ok()?;
        Some([r, g, b])
    } else if trimmed.len() == 6 {
        let r = u8::from_str_radix(&trimmed[0..2], 16).ok()?;
        let g = u8::from_str_radix(&trimmed[2..4], 16).ok()?;
        let b = u8::from_str_radix(&trimmed[4..6], 16).ok()?;
        Some([r, g, b])
    } else {
        None
    }
}

pub(super) fn scene_indices(
    statistics: &czi_rs::SubBlockStatistics,
    summary: &czi_rs::MetadataSummary,
) -> Vec<usize> {
    let mut indices: BTreeSet<usize> = statistics
        .scene_bounding_boxes
        .keys()
        .filter_map(|scene| (*scene >= 0).then_some(*scene as usize))
        .collect();
    if indices.is_empty() {
        let count = summary
            .image
            .sizes
            .get(&CziDimension::S)
            .copied()
            .unwrap_or(1);
        indices.extend(0..count);
    }
    indices.into_iter().collect()
}

pub(super) fn scene_slot_for_subblock(
    scene_coords: &[usize],
    subblock: &czi_rs::DirectorySubBlockInfo,
) -> Option<usize> {
    match subblock.coordinate.get(CziDimension::S) {
        Some(scene) if scene >= 0 => scene_coords
            .iter()
            .position(|candidate| *candidate == scene as usize),
        Some(_) => None,
        None if scene_coords.len() == 1 => Some(0),
        None => None,
    }
}

pub(super) fn subblock_matches_default_plane(
    _subblock: &czi_rs::DirectorySubBlockInfo,
    _statistics: &czi_rs::SubBlockStatistics,
) -> bool {
    true
}

pub(super) fn canvas_dimensions(
    statistics: &czi_rs::SubBlockStatistics,
    summary: &czi_rs::MetadataSummary,
    path: &Path,
) -> Result<(u64, u64), WsiError> {
    let w = summary
        .image
        .sizes
        .get(&CziDimension::X)
        .copied()
        .unwrap_or(0) as u64;
    let h = summary
        .image
        .sizes
        .get(&CziDimension::Y)
        .copied()
        .unwrap_or(0) as u64;
    if w > 0 && h > 0 {
        return Ok((w, h));
    }

    let mut max_x = 0i64;
    let mut max_y = 0i64;
    let mut min_x = 0i64;
    let mut min_y = 0i64;
    let mut seen = false;
    for bounding_boxes in statistics.scene_bounding_boxes.values() {
        let rect = if bounding_boxes.layer0.is_valid() {
            bounding_boxes.layer0
        } else {
            bounding_boxes.all
        };
        if rect.is_valid() {
            if !seen {
                min_x = i64::from(rect.x);
                min_y = i64::from(rect.y);
                seen = true;
            } else {
                min_x = min_x.min(i64::from(rect.x));
                min_y = min_y.min(i64::from(rect.y));
            }
            max_x = max_x.max(i64::from(rect.x) + i64::from(rect.w));
            max_y = max_y.max(i64::from(rect.y) + i64::from(rect.h));
        }
    }

    if seen && max_x > 0 && max_y > 0 {
        return Ok(((max_x - min_x) as u64, (max_y - min_y) as u64));
    }

    Err(invalid_slide(path, "missing Zeiss canvas dimensions"))
}

pub(super) fn canvas_origin(statistics: &czi_rs::SubBlockStatistics) -> (i32, i32) {
    let mut min_x = 0i32;
    let mut min_y = 0i32;
    let mut seen = false;
    for bounding_boxes in statistics.scene_bounding_boxes.values() {
        let rect = if bounding_boxes.layer0.is_valid() {
            bounding_boxes.layer0
        } else {
            bounding_boxes.all
        };
        if rect.is_valid() {
            if !seen {
                min_x = rect.x;
                min_y = rect.y;
                seen = true;
            } else {
                min_x = min_x.min(rect.x);
                min_y = min_y.min(rect.y);
            }
        }
    }
    (min_x, min_y)
}

pub(super) fn subblock_origin(subblocks: &[czi_rs::DirectorySubBlockInfo]) -> (i32, i32) {
    let min_x = subblocks.iter().map(|info| info.rect.x).min().unwrap_or(0);
    let min_y = subblocks.iter().map(|info| info.rect.y).min().unwrap_or(0);
    (min_x, min_y)
}

pub(super) fn common_level_ratios(
    subblocks: &[czi_rs::DirectorySubBlockInfo],
    scene_indices: &[usize],
    statistics: &czi_rs::SubBlockStatistics,
) -> Vec<u32> {
    let mut scene_sets: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); scene_indices.len()];
    for sb in subblocks {
        let Some(scene_slot) = scene_slot_for_subblock(scene_indices, sb) else {
            continue;
        };
        if !subblock_matches_default_plane(sb, statistics) {
            continue;
        }
        if let Some(ratio) = subblock_ratio(sb) {
            scene_sets[scene_slot].insert(ratio);
        }
    }

    let mut common: Option<BTreeSet<u32>> = None;
    for set in scene_sets.into_iter().filter(|set| !set.is_empty()) {
        common = Some(match common {
            Some(prev) => prev.intersection(&set).copied().collect(),
            None => set,
        });
    }

    let mut ratios = common.unwrap_or_else(|| {
        let mut fallback = BTreeSet::new();
        fallback.insert(1);
        fallback
    });
    ratios.insert(1);
    ratios.into_iter().collect()
}

pub(super) fn build_canvas_level_tile_subblocks(
    subblocks: &[czi_rs::DirectorySubBlockInfo],
    canvas_level_subblocks: &[Vec<usize>],
    levels: &[Level],
    subblock_origin: (i32, i32),
) -> Vec<StdHashMap<(i64, i64), Vec<usize>>> {
    let mut out = vec![StdHashMap::<(i64, i64), Vec<usize>>::new(); levels.len()];
    for (level_idx, indices) in canvas_level_subblocks.iter().enumerate() {
        let Some(level) = levels.get(level_idx) else {
            continue;
        };
        let TileLayout::Regular {
            tile_width,
            tile_height,
            ..
        } = level.tile_layout
        else {
            continue;
        };
        let level_ratio = level.downsample.round().max(1.0) as i64;
        let tile_w = i64::from(tile_width);
        let tile_h = i64::from(tile_height);
        for &index in indices {
            let Some(info) = subblocks.get(index) else {
                continue;
            };
            let x = i64::from(info.rect.x - subblock_origin.0).div_euclid(level_ratio);
            let y = i64::from(info.rect.y - subblock_origin.1).div_euclid(level_ratio);
            let w = i64::from(info.stored_size.w);
            let h = i64::from(info.stored_size.h);
            if w <= 0 || h <= 0 {
                continue;
            }
            let start_col = x.div_euclid(tile_w);
            let end_col = (x + w - 1).div_euclid(tile_w);
            let start_row = y.div_euclid(tile_h);
            let end_row = (y + h - 1).div_euclid(tile_h);
            let map = &mut out[level_idx];
            for col in start_col..=end_col {
                for row in start_row..=end_row {
                    map.entry((col, row)).or_default().push(index);
                }
            }
        }
    }
    out
}

pub(super) fn subblock_ratio(subblock: &czi_rs::DirectorySubBlockInfo) -> Option<u32> {
    if subblock.rect.w <= 0
        || subblock.rect.h <= 0
        || subblock.stored_size.w == 0
        || subblock.stored_size.h == 0
    {
        return None;
    }
    let width_ratio = ((subblock.rect.w as f64) / (subblock.stored_size.w as f64)).round() as u32;
    let height_ratio = ((subblock.rect.h as f64) / (subblock.stored_size.h as f64)).round() as u32;
    (width_ratio > 0 && width_ratio == height_ratio).then_some(width_ratio)
}

pub(super) fn build_levels((width, height): (u64, u64), ratios: &[u32]) -> Vec<Level> {
    ratios
        .iter()
        .map(|&ratio| {
            let ratio_u64 = u64::from(ratio);
            let level_w = width / ratio_u64;
            let level_h = height / ratio_u64;
            let tile_width = DEFAULT_TILE_PX;
            let tile_height = DEFAULT_TILE_PX;
            let tiles_across = level_w.div_ceil(u64::from(tile_width));
            let tiles_down = level_h.div_ceil(u64::from(tile_height));
            Level {
                dimensions: (level_w, level_h),
                downsample: ratio as f64,
                tile_layout: TileLayout::Regular {
                    tile_width,
                    tile_height,
                    tiles_across,
                    tiles_down,
                },
            }
        })
        .collect()
}

pub(super) fn quickhash_for_zeiss(
    header: &czi_rs::FileHeaderInfo,
    xml: &str,
) -> Result<String, WsiError> {
    let mut quickhash = Quickhash1::new();
    quickhash.update(&guid_bytes(&header.primary_file_guid)?);
    quickhash.update(&guid_bytes(&header.file_guid)?);
    quickhash.hash_string(xml);
    quickhash
        .finish()
        .ok_or_else(|| WsiError::DisplayConversion("failed to compute Zeiss quickhash".into()))
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

pub(super) fn extract_objective_magnification(xml: &str) -> Option<String> {
    let ref_id = extract_attribute(xml, "ObjectiveRef", "Id")?;
    let objective_marker = format!(r#"<Objective Id="{}""#, ref_id);
    let objective_start = xml.find(&objective_marker)?;
    let objective_xml = &xml[objective_start..];
    extract_tag_text(objective_xml, "NominalMagnification")
}

fn extract_attribute(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag} "))?;
    let rest = &xml[start..];
    let needle = format!(r#"{attr}=""#);
    let attr_start = rest.find(&needle)? + needle.len();
    let attr_end = rest[attr_start..].find('"')? + attr_start;
    Some(rest[attr_start..attr_end].to_string())
}

fn extract_tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let value = xml[start..end].trim();
    (!value.is_empty()).then_some(value.to_string())
}

pub(super) fn invalid_slide(path: &Path, message: impl Into<String>) -> WsiError {
    WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: message.into(),
    }
}
