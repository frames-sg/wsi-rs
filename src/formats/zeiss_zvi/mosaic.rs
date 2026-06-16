use super::model::{MosaicGrid, ZviPlane};
use super::*;

const POSITION_DEDUP_TOLERANCE_PX: i64 = 128;

pub(super) fn apply_mosaic_positions(planes: &mut [ZviPlane], mpp: (f64, f64)) {
    let min_x = planes
        .iter()
        .filter_map(|plane| plane.stage_position.map(|(x, _)| x))
        .fold(f64::INFINITY, f64::min);
    let min_y = planes
        .iter()
        .filter_map(|plane| plane.stage_position.map(|(_, y)| y))
        .fold(f64::INFINITY, f64::min);
    if !(min_x.is_finite() && min_y.is_finite() && mpp.0 > 0.0 && mpp.1 > 0.0) {
        return;
    }
    for plane in planes {
        if let Some((stage_x, stage_y)) = plane.stage_position {
            plane.pixel_offset = (
                ((stage_x - min_x) / mpp.0).round() as i64,
                ((stage_y - min_y) / mpp.1).round() as i64,
            );
        }
    }
}

pub(super) fn build_mosaic_grid(
    planes: &mut [ZviPlane],
    tile_width: u32,
    tile_height: u32,
) -> MosaicGrid {
    let mut tile_offsets = BTreeMap::<i32, (i64, i64)>::new();
    for plane in planes.iter() {
        tile_offsets
            .entry(plane.tile_index)
            .or_insert(plane.pixel_offset);
    }

    let row_positions = dedup_positions(tile_offsets.values().map(|(_, y)| *y).collect::<Vec<_>>());
    let advance_y = median_step(&row_positions).unwrap_or(tile_height as f64);
    let mut row_columns: HashMap<i64, Vec<i64>> = HashMap::new();
    for (x, y) in tile_offsets.values() {
        let row = nearest_position_index(&row_positions, *y) as i64;
        row_columns.entry(row).or_default().push(*x);
    }
    for columns in row_columns.values_mut() {
        *columns = dedup_positions(std::mem::take(columns));
    }
    let advance_x = row_columns
        .values()
        .filter_map(|columns| median_step(columns))
        .next()
        .unwrap_or(tile_width as f64);

    let mut tile_key_by_index = HashMap::<i32, (i64, i64)>::new();
    let mut entries = HashMap::new();
    let mut width = 0u64;
    let mut height = 0u64;
    for (tile_index, (x, y)) in &tile_offsets {
        let row = nearest_position_index(&row_positions, *y) as i64;
        let columns = row_columns.get(&row).cloned().unwrap_or_default();
        let col = nearest_position_index(&columns, *x) as i64;
        tile_key_by_index.insert(*tile_index, (col, row));
        width = width.max((*x).max(0) as u64 + u64::from(tile_width));
        height = height.max((*y).max(0) as u64 + u64::from(tile_height));
        entries.insert(
            (col, row),
            TileEntry {
                offset: (
                    *x as f64 - col as f64 * advance_x,
                    *y as f64 - row as f64 * advance_y,
                ),
                dimensions: (tile_width, tile_height),
                tiff_tile_index: None,
            },
        );
    }

    for plane in planes {
        plane.grid_key = tile_key_by_index.get(&plane.tile_index).copied();
    }

    MosaicGrid {
        advance_x,
        advance_y,
        width,
        height,
        entries,
    }
}

fn dedup_positions(mut values: Vec<i64>) -> Vec<i64> {
    values.sort_unstable();
    let mut out: Vec<i64> = Vec::new();
    for value in values {
        if out
            .last()
            .is_none_or(|last| (value - *last).abs() > POSITION_DEDUP_TOLERANCE_PX)
        {
            out.push(value);
        }
    }
    out
}

fn nearest_position_index(values: &[i64], target: i64) -> usize {
    values
        .iter()
        .enumerate()
        .min_by_key(|(_, value)| (target - **value).abs())
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

fn median_step(values: &[i64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let mut steps = values
        .windows(2)
        .filter_map(|pair| {
            let step = pair[1] - pair[0];
            (step > POSITION_DEDUP_TOLERANCE_PX).then_some(step as f64)
        })
        .collect::<Vec<_>>();
    if steps.is_empty() {
        return None;
    }
    steps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(steps[steps.len() / 2])
}

pub(super) fn build_zvi_channels(planes: &[ZviPlane], size_c: u32) -> Vec<ChannelInfo> {
    (0..size_c)
        .map(|c| {
            let plane = planes.iter().find(|plane| plane.c == c);
            ChannelInfo {
                name: plane.and_then(|plane| plane.channel_name.clone()),
                color: plane.and_then(|plane| plane.channel_color),
                excitation_nm: None,
                emission_nm: None,
            }
        })
        .collect()
}
