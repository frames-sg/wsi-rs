use super::*;

impl Slide {
    /// Where a source tile's encoded bytes are stored in the slide's files,
    /// from the file's index, without decoding the tile. Some formats read a
    /// few marker bytes to place a range exactly (the end of a DICOM frame,
    /// NDPI restart markers, the VMS restart-marker scan).
    ///
    /// The byte ranges can be copied unchanged, cropped losslessly by JPEG
    /// tools, or referenced from another container. Formats without location
    /// support return [`WsiError::Unsupported`].
    pub fn tile_location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        let (_, series, _) = self.dataset().level(req.scene, req.series, req.level)?;
        series.axes.validate_plane(req.plane.get())?;
        self.source.tile_location(req)
    }

    /// Every tile location of one level plane in one call, in row-major
    /// order, for building references to a whole level.
    ///
    /// Regular and whole-level grids list every grid cell; irregular
    /// layouts list their stored entries. The result is bounded by
    /// [`SlideLimits::tile_index_bytes`]. A tile whose location cannot be
    /// read fails the call with an error naming that tile.
    pub fn level_tile_locations(
        &self,
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
        plane: impl Into<PlaneIdx>,
    ) -> Result<LevelTileLocations, WsiError> {
        let (scene, series, level, plane) =
            (scene.into(), series.into(), level.into(), plane.into());
        let (_, series_ref, level_ref) = self.dataset().level(scene, series, level)?;
        series_ref.axes.validate_plane(plane.get())?;
        let grid = LevelGrid::of(&level_ref.tile_layout)?;
        let budget = self.limits.tile_index_bytes();
        let entry_bytes = (std::mem::size_of::<LevelTileLocation>()
            + std::mem::size_of::<EncodedUnit>()
            + std::mem::size_of::<ByteRange>()) as u64;
        let count = grid.count();
        if count
            .checked_mul(entry_bytes)
            .is_none_or(|bytes| bytes > budget)
        {
            return Err(location_budget_error(budget));
        }
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(count as usize)
            .map_err(|err| WsiError::Unsupported {
                reason: format!("tile location list allocation failed: {err}"),
            })?;
        let mut retained = RetainedBytes::default();
        for (col, row) in grid.cells() {
            let req = TileRequest::new(scene, series, level, col, row).with_plane(plane);
            let location = self
                .source
                .tile_location(&req)
                .map_err(|err| WsiError::tile_read(&req, format!("tile location: {err}")))?;
            if retained.add(&location).is_none_or(|bytes| bytes > budget) {
                return Err(location_budget_error(budget));
            }
            tiles.push(LevelTileLocation::new(col, row, location));
        }
        Ok(LevelTileLocations {
            tile_width: grid.tile_width,
            tile_height: grid.tile_height,
            tiles_across: grid.tiles_across,
            tiles_down: grid.tiles_down,
            tiles,
        })
    }
}

fn location_budget_error(budget: u64) -> WsiError {
    WsiError::Unsupported {
        reason: format!(
            "level tile locations exceed the {budget}-byte tile index limit; query tiles individually"
        ),
    }
}

/// Bytes a level listing keeps alive. Prefix, suffix and table buffers are
/// shared between tiles, so each distinct buffer is counted once.
#[derive(Default)]
struct RetainedBytes {
    total: u64,
    shared_buffers: std::collections::HashSet<usize>,
}

impl RetainedBytes {
    /// Adds one listed location; `None` when the total overflows `u64`.
    fn add(&mut self, location: &TileLocation) -> Option<u64> {
        let units = match location {
            TileLocation::Stored(stored) => stored.units(),
            _ => &[],
        };
        let mut bytes = std::mem::size_of::<LevelTileLocation>();
        for unit in units {
            bytes += std::mem::size_of::<EncodedUnit>() + std::mem::size_of_val(unit.segments());
            let shared = [
                unit.prefix(),
                unit.suffix(),
                unit.codec().jpeg_tables().unwrap_or_default(),
            ];
            for buffer in shared {
                if !buffer.is_empty() && self.shared_buffers.insert(buffer.as_ptr() as usize) {
                    bytes += buffer.len();
                }
            }
        }
        self.total = self.total.checked_add(bytes as u64)?;
        Some(self.total)
    }
}

struct LevelGrid {
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
    /// Stored entries of an irregular layout, sorted row-major.
    entries: Option<Vec<(i64, i64)>>,
}

impl LevelGrid {
    fn of(layout: &TileLayout) -> Result<Self, WsiError> {
        match layout {
            TileLayout::Regular {
                tile_width,
                tile_height,
                tiles_across,
                tiles_down,
            } => Ok(Self {
                tile_width: *tile_width,
                tile_height: *tile_height,
                tiles_across: *tiles_across,
                tiles_down: *tiles_down,
                entries: None,
            }),
            TileLayout::WholeLevel {
                width,
                height,
                virtual_tile_width,
                virtual_tile_height,
            } => {
                if *virtual_tile_width == 0 || *virtual_tile_height == 0 {
                    return Err(WsiError::Unsupported {
                        reason: "whole-level layout has a zero virtual tile size".into(),
                    });
                }
                Ok(Self {
                    tile_width: *virtual_tile_width,
                    tile_height: *virtual_tile_height,
                    tiles_across: width.div_ceil(u64::from(*virtual_tile_width)),
                    tiles_down: height.div_ceil(u64::from(*virtual_tile_height)),
                    entries: None,
                })
            }
            TileLayout::Irregular {
                tile_advance,
                tiles,
                ..
            } => {
                let mut entries: Vec<(i64, i64)> = tiles.keys().copied().collect();
                entries.sort_unstable_by_key(|&(col, row)| (row, col));
                let extent = |values: &mut dyn Iterator<Item = i64>| {
                    let (min, max) = values.fold((i64::MAX, i64::MIN), |(lo, hi), value| {
                        (lo.min(value), hi.max(value))
                    });
                    if min > max {
                        0
                    } else {
                        max.abs_diff(min) + 1
                    }
                };
                Ok(Self {
                    tile_width: tile_advance.0.ceil().clamp(0.0, f64::from(u32::MAX)) as u32,
                    tile_height: tile_advance.1.ceil().clamp(0.0, f64::from(u32::MAX)) as u32,
                    tiles_across: extent(&mut entries.iter().map(|&(col, _)| col)),
                    tiles_down: extent(&mut entries.iter().map(|&(_, row)| row)),
                    entries: Some(entries),
                })
            }
        }
    }

    fn count(&self) -> u64 {
        match &self.entries {
            Some(entries) => entries.len() as u64,
            None => self.tiles_across.saturating_mul(self.tiles_down),
        }
    }

    fn cells(&self) -> Box<dyn Iterator<Item = (i64, i64)> + '_> {
        match &self.entries {
            Some(entries) => Box::new(entries.iter().copied()),
            None => {
                let across = i64::try_from(self.tiles_across).unwrap_or(i64::MAX);
                let down = i64::try_from(self.tiles_down).unwrap_or(i64::MAX);
                Box::new((0..down).flat_map(move |row| (0..across).map(move |col| (col, row))))
            }
        }
    }
}

#[cfg(test)]
mod tests;
