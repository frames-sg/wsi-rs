use super::*;

const MAX_VENTANA_TILES_PER_LEVEL: u64 = 1_000_000;

// ── Level 0 XML parsing ────────────────────────────────────────────

/// Parsed BIF area of interest.
pub(super) struct BifArea {
    pub(super) x: i64,
    pub(super) y: i64,
    pub(super) start_col: i64,
    pub(super) start_row: i64,
    pub(super) tiles_across: i64,
    pub(super) tiles_down: i64,
}

pub(super) struct BifTile {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: u32,
    pub(super) height: u32,
}

/// Parsed BIF layout metadata.
pub(super) struct BifInfo {
    pub(super) areas: Vec<BifArea>,
    pub(super) tiles: Vec<BifTile>,
    pub(super) tile_advance_x: f64,
    pub(super) tile_advance_y: f64,
}

/// Parse the EncodeInfo XML for BIF tile layout.
pub(super) fn parse_level0_xml(
    xml_str: &str,
    tile_width: i64,
    tile_height: i64,
) -> Result<BifInfo, TiffParseError> {
    if tile_width <= 0 || tile_height <= 0 {
        return Err(TiffParseError::Structure(format!(
            "Ventana BIF: tile dimensions must be greater than zero (got {tile_width}x{tile_height})"
        )));
    }
    let root = xml::parse_xml(xml_str)
        .map_err(|e| TiffParseError::Structure(format!("Ventana BIF: XML parse error: {}", e)))?;

    let slide_info = root.find("SlideStitchInfo").ok_or_else(|| {
        TiffParseError::Structure("Ventana BIF: no SlideStitchInfo in EncodeInfo XML".into())
    })?;
    let image_infos = slide_info.find_all("ImageInfo");
    let origin_infos = root
        .find("AoiOrigin")
        .map(|node| node.children.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    if !origin_infos.is_empty() && origin_infos.len() != image_infos.len() {
        return Err(TiffParseError::Structure(format!(
            "Ventana BIF: mismatched AOI/ImageInfo counts ({} vs {})",
            origin_infos.len(),
            image_infos.len()
        )));
    }

    let mut areas = Vec::new();
    let mut tiles = Vec::new();
    let mut total_offset_x: f64 = 0.0;
    let mut total_offset_y: f64 = 0.0;
    let mut total_x_weight: i64 = 0;
    let mut total_y_weight: i64 = 0;
    let mut total_tile_count = 0u64;
    for (idx, info) in image_infos.into_iter().enumerate() {
        let aoi_scanned: i64 = info
            .attr("AOIScanned")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if aoi_scanned == 0 {
            continue;
        }
        let aoi = origin_infos.get(idx).copied();

        let num_cols: i64 = info
            .attr("NumCols")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let num_rows: i64 = info
            .attr("NumRows")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let area_tile_count = checked_ventana_tile_grid(num_cols, num_rows)?;
        total_tile_count = total_tile_count
            .checked_add(area_tile_count)
            .ok_or_else(|| {
                TiffParseError::Structure("Ventana BIF: aggregate tile grid overflow".into())
            })?;
        if total_tile_count > MAX_VENTANA_TILES_PER_LEVEL {
            return Err(TiffParseError::Structure(format!(
                "Ventana BIF: tile grid declares {total_tile_count} tiles, exceeding the {MAX_VENTANA_TILES_PER_LEVEL}-tile safety limit"
            )));
        }
        let pos_x: f64 = info
            .attr("Pos-X")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let pos_y: f64 = info
            .attr("Pos-Y")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let image_width: i64 = info.attr("Width").and_then(|s| s.parse().ok()).unwrap_or(0);
        let image_height: i64 = info
            .attr("Height")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let start_col_x: i64 = aoi
            .and_then(|node| node.attr("OriginX"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let start_row_y: i64 = aoi
            .and_then(|node| node.attr("OriginY"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if start_col_x % tile_width != 0 || start_row_y % tile_height != 0 {
            return Err(TiffParseError::Structure(format!(
                "Ventana BIF: area origin not divisible by tile size: {} % {}, {} % {}",
                start_col_x, tile_width, start_row_y, tile_height
            )));
        }
        let start_col = start_col_x / tile_width;
        let start_row = start_row_y / tile_height;
        start_col.checked_add(num_cols).ok_or_else(|| {
            TiffParseError::Structure("Ventana BIF: tile grid column range overflow".into())
        })?;
        start_row.checked_add(num_rows).ok_or_else(|| {
            TiffParseError::Structure("Ventana BIF: tile grid row range overflow".into())
        })?;

        // Accumulate joint offsets for tile advance computation.
        for joint_info in info.find_all("TileJointInfo") {
            let overlap_x: f64 = joint_info
                .attr("OverlapX")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let overlap_y: f64 = joint_info
                .attr("OverlapY")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let confidence: i64 = joint_info
                .attr("Confidence")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let direction = joint_info.attr("Direction").unwrap_or("");

            if direction == "UP" {
                total_offset_y += confidence as f64 * (-overlap_y);
                total_y_weight += confidence;
            } else {
                total_offset_x += confidence as f64 * (-overlap_x);
                total_x_weight += confidence;
            }
        }

        areas.push(BifArea {
            x: pos_x as i64,
            y: pos_y as i64,
            start_col,
            start_row,
            tiles_across: num_cols,
            tiles_down: num_rows,
        });

        let exact_positions = parse_area_tile_positions(
            info,
            num_cols,
            num_rows,
            tile_width as f64,
            tile_height as f64,
        );
        let exact_position_map = exact_positions
            .iter()
            .map(|(tile_id, (tile_x, tile_y))| {
                let (local_col, local_row) = ventana_snake_coords(*tile_id, num_cols);
                ((local_col, local_row), (*tile_x, *tile_y))
            })
            .collect::<HashMap<_, _>>();
        for (tile_id, tile_pos) in exact_positions {
            let (tile_x, tile_y) = tile_pos;
            let (local_col, local_row) = ventana_snake_coords(tile_id, num_cols);
            let (width, height) = ventana_exact_tile_dimensions(
                local_col,
                local_row,
                num_cols,
                num_rows,
                &exact_position_map,
                image_width as f64,
                image_height as f64,
                tile_width as f64,
                tile_height as f64,
            );
            tiles.push(BifTile {
                x: pos_x + tile_x,
                y: pos_y + tile_y,
                width,
                height,
            });
        }
    }

    let tile_advance_x = if total_x_weight > 0 {
        tile_width as f64 + total_offset_x / total_x_weight as f64
    } else {
        tile_width as f64
    };
    let tile_advance_y = if total_y_weight > 0 {
        tile_height as f64 + total_offset_y / total_y_weight as f64
    } else {
        tile_height as f64
    };

    let mut top = 0i64;
    let heights = areas
        .iter()
        .map(|area| {
            let height =
                ((area.tiles_down - 1) as f64 * tile_advance_y + tile_height as f64).round() as i64;
            top = top.max(area.y + height);
            height
        })
        .collect::<Vec<_>>();
    for (area, height) in areas.iter_mut().zip(heights) {
        area.y = top - area.y - height;
    }

    Ok(BifInfo {
        areas,
        tiles,
        tile_advance_x,
        tile_advance_y,
    })
}

fn checked_ventana_tile_grid(num_cols: i64, num_rows: i64) -> Result<u64, TiffParseError> {
    let (Ok(num_cols), Ok(num_rows)) = (u64::try_from(num_cols), u64::try_from(num_rows)) else {
        return Err(TiffParseError::Structure(format!(
            "Ventana BIF: tile grid dimensions must be positive (got {num_cols}x{num_rows})"
        )));
    };
    if num_cols == 0 || num_rows == 0 {
        return Err(TiffParseError::Structure(format!(
            "Ventana BIF: tile grid dimensions must be positive (got {num_cols}x{num_rows})"
        )));
    }
    let tile_count = num_cols
        .checked_mul(num_rows)
        .ok_or_else(|| TiffParseError::Structure("Ventana BIF: tile grid size overflow".into()))?;
    if tile_count > MAX_VENTANA_TILES_PER_LEVEL {
        return Err(TiffParseError::Structure(format!(
            "Ventana BIF: tile grid declares {tile_count} tiles, exceeding the {MAX_VENTANA_TILES_PER_LEVEL}-tile safety limit"
        )));
    }
    Ok(tile_count)
}

fn parse_area_tile_positions(
    info: &xml::XmlNode,
    num_cols: i64,
    num_rows: i64,
    tile_width: f64,
    tile_height: f64,
) -> Vec<(i64, (f64, f64))> {
    let tile_count = num_cols.max(0) * num_rows.max(0);
    if tile_count == 0 {
        return Vec::new();
    }

    let mut edges: HashMap<i64, Vec<(i64, f64, f64)>> = HashMap::new();
    let mut seed_tile = None;

    for joint_info in info.find_all("TileJointInfo") {
        let Some(tile1) = joint_info.attr("Tile1").and_then(|s| s.parse::<i64>().ok()) else {
            continue;
        };
        let Some(tile2) = joint_info.attr("Tile2").and_then(|s| s.parse::<i64>().ok()) else {
            continue;
        };
        if tile1 <= 0 || tile2 <= 0 || tile1 > tile_count || tile2 > tile_count {
            continue;
        }
        seed_tile.get_or_insert(tile1);

        let overlap_x = joint_info
            .attr("OverlapX")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let overlap_y = joint_info
            .attr("OverlapY")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let Some((dx, dy)) = joint_delta(
            joint_info.attr("Direction").unwrap_or(""),
            tile_width,
            tile_height,
            overlap_x,
            overlap_y,
        ) else {
            continue;
        };

        edges.entry(tile1).or_default().push((tile2, dx, dy));
        edges.entry(tile2).or_default().push((tile1, -dx, -dy));
    }

    let mut positions: HashMap<i64, (f64, f64)> = HashMap::new();
    let mut queue = std::collections::VecDeque::new();
    let root = seed_tile.unwrap_or(1);
    positions.insert(root, (0.0, 0.0));
    queue.push_back(root);

    while let Some(tile_id) = queue.pop_front() {
        let Some((tile_x, tile_y)) = positions.get(&tile_id).copied() else {
            continue;
        };
        for &(neighbor, dx, dy) in edges.get(&tile_id).into_iter().flatten() {
            if positions.contains_key(&neighbor) {
                continue;
            }
            positions.insert(neighbor, (tile_x + dx, tile_y + dy));
            queue.push_back(neighbor);
        }
    }

    for tile_id in 1..=tile_count {
        positions.entry(tile_id).or_insert_with(|| {
            let (col, row) = ventana_snake_coords(tile_id, num_cols);
            (col as f64 * tile_width, row as f64 * tile_height)
        });
    }

    let min_x = positions
        .values()
        .map(|(x, _)| *x)
        .fold(f64::INFINITY, f64::min);
    let min_y = positions
        .values()
        .map(|(_, y)| *y)
        .fold(f64::INFINITY, f64::min);

    let mut result = positions.into_iter().collect::<Vec<_>>();
    result.sort_by_key(|(tile_id, _)| *tile_id);
    for (_, (x, y)) in &mut result {
        *x -= min_x;
        *y -= min_y;
    }
    result
}

pub(super) fn joint_delta(
    direction: &str,
    tile_width: f64,
    tile_height: f64,
    overlap_x: f64,
    overlap_y: f64,
) -> Option<(f64, f64)> {
    match direction {
        "RIGHT" => Some((tile_width - overlap_x, overlap_y)),
        "LEFT" => Some((-(tile_width - overlap_x), overlap_y)),
        "UP" => Some((overlap_x, tile_height - overlap_y)),
        "DOWN" => Some((overlap_x, -(tile_height - overlap_y))),
        _ => None,
    }
}

pub(super) fn ventana_snake_coords(tile_id: i64, num_cols: i64) -> (i64, i64) {
    let zero_based = tile_id - 1;
    let row = zero_based.div_euclid(num_cols);
    let col_in_row = zero_based.rem_euclid(num_cols);
    let col = if row % 2 == 0 {
        col_in_row
    } else {
        num_cols - 1 - col_in_row
    };
    (col, row)
}
