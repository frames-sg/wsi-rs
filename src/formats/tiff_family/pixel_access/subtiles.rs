use super::*;
use crate::core::registry::cairo_subtile_surface_u8;

impl TiffPixelReader {
    /// Reads a batch of Ventana reduced-level cells grouped by stored tile.
    /// Region hits arrive in row-major cell order, which alternates between
    /// neighbouring stored tiles; grouping decodes each stored tile once per
    /// batch even when the private cache holds only one of them.
    pub(super) fn read_tiled_ifd_subtiles_grouped(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
    ) -> Result<Vec<CpuTile>, WsiError> {
        let groups = reqs
            .iter()
            .map(|req| self.stored_tile_group(req))
            .collect::<Result<Vec<_>, WsiError>>()?;
        let mut order: Vec<usize> = (0..reqs.len()).collect();
        order.sort_by_key(|&index| groups[index]);
        let mut tiles: Vec<Option<CpuTile>> = vec![None; reqs.len()];
        for index in order {
            tiles[index] = Some(self.read_tile_cpu_with_backend_request(&reqs[index], backend)?);
        }
        Ok(tiles.into_iter().flatten().collect())
    }

    /// The `(IFD, stored row, stored column)` a subtile request crops, or
    /// `None` for requests of any other source.
    fn stored_tile_group(&self, req: &TileRequest) -> Result<Option<(u64, u64, u64)>, WsiError> {
        let TileSource::TiledIfdSubtiles {
            ifd_id,
            subtiles_per_tile,
            ..
        } = self.tile_source_for(req)?
        else {
            return Ok(None);
        };
        let per_tile = u64::from(*subtiles_per_tile).max(1);
        Ok(u64::try_from(req.col)
            .ok()
            .zip(u64::try_from(req.row).ok())
            .map(|(col, row)| (ifd_id.0, row / per_tile, col / per_tile)))
    }

    /// Reads one Ventana reduced-level cell as OpenSlide paints it: the
    /// `1 / subtiles_per_tile` share of a stored TIFF tile. The stored tile is
    /// decoded once into the private cache because a thumbnail touches every
    /// level-0 cell, and many cells share one stored tile.
    pub(super) fn read_tiled_ifd_subtile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        jpeg_tables: Option<&[u8]>,
        compression: Compression,
        subtiles_per_tile: u32,
    ) -> Result<CpuTile, WsiError> {
        let tile_error = |reason: String| WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason,
        };
        let entry = self.subtile_entry(req)?;
        let (Ok(col), Ok(row)) = (u64::try_from(req.col), u64::try_from(req.row)) else {
            return Err(tile_error(
                "Ventana subtile coordinates must be non-negative".into(),
            ));
        };
        if subtiles_per_tile == 0 {
            return Err(tile_error(
                "Ventana subtile divisor must be positive".into(),
            ));
        }
        let geometry = self.stored_tile_geometry(ifd_id).map_err(tile_error)?;
        let per_tile = u64::from(subtiles_per_tile);
        let (tile_col, tile_row) = (col / per_tile, row / per_tile);
        if tile_col >= geometry.tiles_across || tile_row >= geometry.tiles_down {
            return Err(tile_error(format!(
                "stored tile ({tile_col},{tile_row}) is outside the {}x{} TIFF tile grid",
                geometry.tiles_across, geometry.tiles_down
            )));
        }
        let index = usize::try_from(tile_row * geometry.tiles_across + tile_col)
            .map_err(|_| tile_error("stored tile index overflows usize".into()))?;
        let clipped = (
            (geometry.width - tile_col * u64::from(geometry.tile_width))
                .min(u64::from(geometry.tile_width)) as u32,
            (geometry.height - tile_row * u64::from(geometry.tile_height))
                .min(u64::from(geometry.tile_height)) as u32,
        );
        let stored = self.full_decode_cache.get_or_try_insert_with_error(
            FullDecodeKey::StoredTile { ifd_id, index },
            || {
                let (offsets, byte_counts) = self.tiled_ifd_offsets_and_byte_counts(ifd_id)?;
                self.decode_tiled_ifd_tile_index(
                    ifd_id,
                    index,
                    jpeg_tables,
                    compression,
                    clipped.0,
                    clipped.1,
                    offsets,
                    byte_counts,
                    BackendRequest::Cpu,
                )
                .map(Arc::new)
            },
            tile_error,
        );
        let stored = stored.map_err(|err| match err {
            WsiError::TileRead { .. } => err,
            other => tile_error(other.to_string()),
        })?;

        let scale = f64::from(subtiles_per_tile);
        let extent = (
            f64::from(geometry.tile_width) / scale,
            f64::from(geometry.tile_height) / scale,
        );
        let origin = (
            (col % per_tile) as f64 * extent.0,
            (row % per_tile) as f64 * extent.1,
        );
        let subtile = if !entry.has_explicit_extent() {
            // Whole-pixel subtiles are plain crops; clipped stored tiles leave
            // the uncovered remainder to the compositor as transparent.
            let x = origin.0 as u32;
            let y = origin.1 as u32;
            crop_rgb_interleaved_u8_buffer(
                &stored,
                x,
                y,
                entry.dimensions.0.min(stored.width.saturating_sub(x)),
                entry.dimensions.1.min(stored.height.saturating_sub(y)),
            )
        } else {
            cairo_subtile_surface_u8(&stored, origin, entry.dimensions)
        };
        subtile.map_err(|err| tile_error(err.to_string()))
    }

    fn subtile_entry(&self, req: &TileRequest) -> Result<&TileEntry, WsiError> {
        let level = self
            .layout
            .dataset
            .scenes
            .get(req.scene.get())
            .and_then(|scene| scene.series.get(req.series.get()))
            .and_then(|series| series.levels.get(req.level.get() as usize))
            .ok_or(WsiError::LevelOutOfRange {
                level: req.level.get(),
                count: 0,
            })?;
        let TileLayout::Irregular { tiles, .. } = &level.tile_layout else {
            return Err(WsiError::UnsupportedFormat(
                "Ventana subtile levels must use irregular tilemaps".into(),
            ));
        };
        tiles
            .get(&(req.col, req.row))
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("no Ventana subtile at ({},{})", req.col, req.row),
            })
    }

    fn stored_tile_geometry(&self, ifd_id: IfdId) -> Result<StoredTileGeometry, String> {
        let read_u64 = |tag, name| {
            self.container
                .get_u64(ifd_id, tag)
                .map_err(|err| format!("failed to read stored TIFF {name}: {err}"))
        };
        let read_u32 = |tag, name| {
            self.container
                .get_u32(ifd_id, tag)
                .map_err(|err| format!("failed to read stored TIFF {name}: {err}"))
        };
        let width = read_u64(tags::IMAGE_WIDTH, "image width")?;
        let height = read_u64(tags::IMAGE_LENGTH, "image height")?;
        let tile_width = read_u32(tags::TILE_WIDTH, "tile width")?;
        let tile_height = read_u32(tags::TILE_LENGTH, "tile height")?;
        if tile_width == 0 || tile_height == 0 {
            return Err(format!(
                "stored TIFF tile size {tile_width}x{tile_height} is empty"
            ));
        }
        Ok(StoredTileGeometry {
            width,
            height,
            tile_width,
            tile_height,
            tiles_across: width.div_ceil(u64::from(tile_width)),
            tiles_down: height.div_ceil(u64::from(tile_height)),
        })
    }
}

struct StoredTileGeometry {
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
}
