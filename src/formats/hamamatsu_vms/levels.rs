use super::model::VmsLevel;
use super::*;

const VMS_SCALES: [u32; 3] = [2, 4, 8];

pub(super) fn expanded_levels(base_level: VmsLevel, map_level: VmsLevel) -> Vec<VmsLevel> {
    let mut levels_by_width = BTreeMap::new();
    for level in [base_level, map_level] {
        insert_scaled_levels(&mut levels_by_width, level);
    }
    levels_by_width
        .into_iter()
        .rev()
        .map(|(_, level)| level)
        .collect()
}

fn insert_scaled_levels(levels: &mut BTreeMap<u64, VmsLevel>, level: VmsLevel) {
    let width = base_level_dimensions(&level).0;
    levels.insert(width, level);
    let original = levels.get(&width).unwrap().clone_for_scale_base();
    for scale in VMS_SCALES {
        let tile_width = original.jpegs[0].tile_width;
        let tile_height = original.jpegs[0].tile_height;
        if !tile_width.is_multiple_of(scale) || !tile_height.is_multiple_of(scale) {
            continue;
        }
        levels.insert(
            base_level_dimensions(&original).0 / scale as u64,
            VmsLevel {
                scale_denom: scale,
                jpegs: original.jpegs.clone(),
                jpegs_across: original.jpegs_across,
                base_tiles_across: original.base_tiles_across,
                base_tiles_down: original.base_tiles_down,
            },
        );
    }
}

impl VmsLevel {
    fn clone_for_scale_base(&self) -> Self {
        Self {
            scale_denom: self.scale_denom,
            jpegs: self.jpegs.clone(),
            jpegs_across: self.jpegs_across,
            base_tiles_across: self.base_tiles_across,
            base_tiles_down: self.base_tiles_down,
        }
    }
}

pub(super) fn base_level_dimensions(level: &VmsLevel) -> (u64, u64) {
    let row_width: u64 = level
        .jpegs
        .iter()
        .take(level.jpegs_across as usize)
        .map(|jpeg| u64::from(jpeg.width))
        .sum();
    let col_height: u64 = level
        .jpegs
        .iter()
        .step_by(level.jpegs_across as usize)
        .map(|jpeg| u64::from(jpeg.height))
        .sum();
    (
        row_width / u64::from(level.scale_denom),
        col_height / u64::from(level.scale_denom),
    )
}

pub(super) fn total_tiles_across(level: &VmsLevel) -> u64 {
    level
        .jpegs
        .iter()
        .take(level.jpegs_across as usize)
        .map(|jpeg| u64::from(jpeg.tiles_across))
        .sum()
}

pub(super) fn total_tiles_down(level: &VmsLevel) -> u64 {
    level
        .jpegs
        .iter()
        .step_by(level.jpegs_across as usize)
        .map(|jpeg| u64::from(jpeg.tiles_down))
        .sum()
}
