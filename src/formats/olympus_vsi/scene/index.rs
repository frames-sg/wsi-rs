//! Validated ETS chunk records and their pyramid geometry.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, ReadBytesExt};

use crate::core::registry::OpenBudget;
use crate::core::types::AxesShape;
use crate::error::WsiError;

use super::super::invalid_slide;
use super::header::{EtsHeader, MAX_ETS_DIMENSIONS, MAX_ETS_TILES};
use super::{EtsLevel, EtsTile, EtsTileKey};

pub(in crate::formats::olympus_vsi) const MAX_ETS_LEVEL_INDEX: u32 = 1_023;
pub(in crate::formats::olympus_vsi) const MAX_ETS_AXIS_INDEX: u32 = 65_535;

pub(super) struct EtsIndex {
    pub(super) tiles: HashMap<EtsTileKey, EtsTile>,
    max_level: u32,
    max_z: u32,
    max_c: u32,
    max_t: u32,
}

impl EtsIndex {
    pub(super) fn read(
        file: &mut (impl Read + Seek),
        path: &Path,
        budget: &OpenBudget,
        header: &EtsHeader,
    ) -> Result<Self, WsiError> {
        let EtsHeader {
            file_len,
            n_dimensions,
            used_chunk_offset,
            n_used_chunks,
            use_pyramid,
            ..
        } = *header;
        file.seek(SeekFrom::Start(used_chunk_offset))?;
        let tile_index_limit = u64::from(MAX_ETS_TILES)
            .saturating_mul(
                u64::try_from(std::mem::size_of::<(EtsTileKey, EtsTile)>()).unwrap_or(u64::MAX),
            )
            .min(budget.limits().tile_index_bytes());
        let tile_index_bytes = u64::from(n_used_chunks).saturating_mul(
            u64::try_from(std::mem::size_of::<(EtsTileKey, EtsTile)>()).unwrap_or(u64::MAX),
        );
        budget.retain_index(tile_index_bytes)?;
        let mut tiles = HashMap::new();
        tiles
            .try_reserve(n_used_chunks as usize)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "Olympus ETS tile index",
                requested: tile_index_bytes,
                limit: tile_index_limit,
            })?;
        let mut max_level = 0u32;
        let mut max_z = 0u32;
        let mut max_c = 0u32;
        let mut max_t = 0u32;
        // Keep scalar fields and reserved padding in one bounded read window.
        // seek_relative consumes buffered padding without another file seek.
        let mut buffered = std::io::BufReader::with_capacity(8 * 1024, file);
        let file = &mut buffered;
        let mut coords = Vec::new();
        for _ in 0..n_used_chunks {
            file.seek_relative(4)?;
            let coordinate_count = n_dimensions as usize;
            coords.clear();
            coords
                .try_reserve_exact(coordinate_count)
                .map_err(|_| WsiError::ResourceLimit {
                    resource: "Olympus ETS chunk coordinates",
                    requested: u64::from(n_dimensions).saturating_mul(4),
                    limit: u64::from(MAX_ETS_DIMENSIONS).saturating_mul(4),
                })?;
            for _ in 0..n_dimensions {
                coords.push(file.read_i32::<LittleEndian>()?);
            }
            let offset = file.read_u64::<LittleEndian>()?;
            let byte_count = file.read_u32::<LittleEndian>()?;
            file.seek_relative(4)?;

            let key = key_from_coords(&coords, use_pyramid)?;
            checked_ets_level_count(key.level).map_err(|message| invalid_slide(path, message))?;
            for (name, value) in [("z", key.z), ("c", key.c), ("t", key.t)] {
                checked_ets_axis_len(value, name)
                    .map_err(|message| invalid_slide(path, message))?;
            }
            if byte_count == 0 {
                return Err(invalid_slide(path, "ETS tile payload is empty"));
            }
            if u64::from(byte_count) > budget.limits().encoded_unit_bytes() {
                return Err(WsiError::ResourceLimit {
                    resource: "encoded tile/frame unit",
                    requested: u64::from(byte_count),
                    limit: budget.limits().encoded_unit_bytes(),
                });
            }
            let tile_end = offset
                .checked_add(u64::from(byte_count))
                .ok_or_else(|| invalid_slide(path, "ETS tile payload range overflows"))?;
            if tile_end > file_len {
                return Err(invalid_slide(
                    path,
                    format!(
                        "ETS tile payload range {offset}..{tile_end} exceeds file length {file_len}"
                    ),
                ));
            }
            max_level = max_level.max(key.level);
            max_z = max_z.max(key.z);
            max_c = max_c.max(key.c);
            max_t = max_t.max(key.t);
            if tiles.insert(key, EtsTile { offset, byte_count }).is_some() {
                return Err(invalid_slide(path, "duplicate ETS tile coordinates"));
            }
        }

        Ok(Self {
            tiles,
            max_level,
            max_z,
            max_c,
            max_t,
        })
    }

    pub(super) fn levels(
        &self,
        path: &Path,
        budget: &OpenBudget,
        tile_width: u32,
        tile_height: u32,
    ) -> Result<Vec<EtsLevel>, WsiError> {
        let max_level = self.max_level;
        let tiles = &self.tiles;
        let level_count =
            checked_ets_level_count(max_level).map_err(|message| invalid_slide(path, message))?;
        let level_index_bytes = u64::try_from(level_count)
            .unwrap_or(u64::MAX)
            .saturating_mul(8);
        budget.retain_index(level_index_bytes)?;
        let mut max_col_by_level = Vec::new();
        max_col_by_level
            .try_reserve_exact(level_count)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "Olympus ETS level index",
                requested: level_index_bytes,
                limit: budget.limits().tile_index_bytes(),
            })?;
        max_col_by_level.resize(level_count, 0u32);
        let mut max_row_by_level = Vec::new();
        max_row_by_level
            .try_reserve_exact(level_count)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "Olympus ETS level index",
                requested: level_index_bytes,
                limit: budget.limits().tile_index_bytes(),
            })?;
        max_row_by_level.resize(level_count, 0u32);
        for key in tiles.keys() {
            let idx = key.level as usize;
            max_col_by_level[idx] = max_col_by_level[idx].max(key.col);
            max_row_by_level[idx] = max_row_by_level[idx].max(key.row);
        }

        let levels = max_col_by_level
            .into_iter()
            .zip(max_row_by_level)
            .map(|(max_col, max_row)| {
                Ok(EtsLevel {
                    width: checked_ets_extent(max_col, tile_width, "width")?,
                    height: checked_ets_extent(max_row, tile_height, "height")?,
                    tile_width,
                    tile_height,
                    tiles_across: max_col + 1,
                    tiles_down: max_row + 1,
                })
            })
            .collect::<Result<Vec<_>, String>>()
            .map_err(|message| invalid_slide(path, message))?;

        Ok(levels)
    }

    pub(super) fn axes(&self, path: &Path) -> Result<AxesShape, WsiError> {
        let (max_z, max_c, max_t) = (self.max_z, self.max_c, self.max_t);
        let axes = AxesShape {
            z: checked_ets_axis_len(max_z, "z").map_err(|message| invalid_slide(path, message))?,
            c: checked_ets_axis_len(max_c, "c").map_err(|message| invalid_slide(path, message))?,
            t: checked_ets_axis_len(max_t, "t").map_err(|message| invalid_slide(path, message))?,
        };

        Ok(axes)
    }
}

pub(in crate::formats::olympus_vsi) fn checked_ets_level_count(
    max_level: u32,
) -> Result<usize, String> {
    if max_level > MAX_ETS_LEVEL_INDEX {
        return Err(format!(
            "ETS level index {max_level} exceeds the supported maximum {MAX_ETS_LEVEL_INDEX}"
        ));
    }
    usize::try_from(max_level + 1)
        .map_err(|_| "ETS level count is not addressable on this platform".into())
}

pub(in crate::formats::olympus_vsi) fn checked_ets_axis_len(
    max_index: u32,
    name: &str,
) -> Result<u32, String> {
    if max_index > MAX_ETS_AXIS_INDEX {
        return Err(format!(
            "ETS {name} index {max_index} exceeds the supported maximum {MAX_ETS_AXIS_INDEX}"
        ));
    }
    max_index
        .checked_add(1)
        .ok_or_else(|| format!("ETS {name} axis length overflows"))
}

pub(in crate::formats::olympus_vsi) fn checked_ets_extent(
    max_tile_index: u32,
    tile_size: u32,
    name: &str,
) -> Result<u32, String> {
    max_tile_index
        .checked_add(1)
        .and_then(|tile_count| tile_count.checked_mul(tile_size))
        .ok_or_else(|| format!("ETS {name} overflows 32-bit dimensions"))
}

pub(in crate::formats::olympus_vsi) fn key_from_coords(
    coords: &[i32],
    use_pyramid: bool,
) -> Result<EtsTileKey, WsiError> {
    if coords.len() < 3 {
        return Err(invalid_slide(
            Path::new(""),
            "ETS coordinate dimensionality is too small",
        ));
    }
    let upper = if use_pyramid {
        coords.len().saturating_sub(1)
    } else {
        coords.len()
    };
    let level = if use_pyramid {
        checked_coord(coords[coords.len() - 1], "resolution")?
    } else {
        0
    };
    let extra = &coords[2..upper];
    let z = extra
        .first()
        .copied()
        .map(|value| checked_coord(value, "z"))
        .transpose()?
        .unwrap_or(0);
    let c = extra
        .get(1)
        .copied()
        .map(|value| checked_coord(value, "c"))
        .transpose()?
        .unwrap_or(0);
    let t = extra
        .get(2)
        .copied()
        .map(|value| checked_coord(value, "t"))
        .transpose()?
        .unwrap_or(0);
    Ok(EtsTileKey {
        level,
        z,
        c,
        t,
        col: checked_coord(coords[0], "x")?,
        row: checked_coord(coords[1], "y")?,
    })
}

fn checked_coord(value: i32, name: &str) -> Result<u32, WsiError> {
    u32::try_from(value).map_err(|_| WsiError::InvalidSlide {
        path: PathBuf::new(),
        message: format!("negative ETS {name} coordinate {value}"),
    })
}

#[cfg(test)]
#[path = "index/tests/io.rs"]
mod io_tests;
