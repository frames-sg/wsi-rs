use super::tiled_ifd::{is_lossless_tiff_codec, uncompressed_raw_tile};
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
        let tile_error = |reason: String| WsiError::tile_read(req, reason);
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

    /// Raw passthrough of a Ventana reduced-level cell. JPEG stored tiles
    /// return a lossless crop of the cell's share, which starts at the MCU
    /// boundary at or before the share, or the whole stored JPEG when the
    /// crop does not fit the lossless-crop limit; lossless stored tiles return the
    /// cell's samples as `Compression::None`; JPEG XR and J2K stored tiles
    /// return the whole stored codestream. Where the output is larger than
    /// the cell, [`RawCompressedTile::tile_region`] locates the cell in it.
    pub(super) fn read_tiled_ifd_raw_subtile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        compression: Compression,
        jpeg_tables: Option<&[u8]>,
    ) -> Result<RawCompressedTile, WsiError> {
        let unit = match self.tiff_tile_location(req)? {
            TileLocation::Stored(stored) => match stored.units() {
                [unit] => unit.clone(),
                _ => {
                    return Err(WsiError::Unsupported {
                        reason: "Ventana subtiles of planar stored tiles cannot be passed \
                                 through; read their byte ranges through tile_location"
                            .into(),
                    })
                }
            },
            TileLocation::Absent => {
                return Err(WsiError::Unsupported {
                    reason: "the Ventana stored tile is empty; tile_location reports it as Absent"
                        .into(),
                })
            }
            other => {
                return Err(WsiError::Unsupported {
                    reason: format!(
                        "Ventana subtile pixels are resampled from their stored tile, so no \
                         stored bytes hold them ({other:?})"
                    ),
                })
            }
        };
        if is_lossless_tiff_codec(compression) {
            let raw = uncompressed_raw_tile(&self.read_tile_cpu(req)?)?;
            self.check_raw_output_limit(raw.data().len(), "uncompressed raw tile")?;
            return Ok(raw);
        }
        if compression == Compression::Jpeg {
            return self.tiff_jpeg_units_raw_tile(
                ifd_id,
                std::slice::from_ref(&unit),
                unit.source_region(),
                jpeg_tables,
            );
        }
        if !matches!(
            compression,
            Compression::Jp2kRgb | Compression::Jp2kYcbcr | Compression::JpegXr
        ) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "{compression:?} Ventana stored tiles cannot be passed through; \
                     tile_location reports the stored tile with the subtile's source region"
                ),
            });
        }
        let data = self.read_single_segment(&unit)?;
        let (stored_width, stored_height) = unit.stored_size();
        let codec = unit.codec();
        Ok(RawCompressedTile::builder(compression)
            .dimensions(stored_width, stored_height)
            .bits_allocated(codec.bits_per_sample())
            .samples_per_pixel(codec.samples_per_pixel())
            .photometric_interpretation(codec.photometric_interpretation())
            .tile_region(unit.source_region())
            .data(data)
            .build()?)
    }

    pub(super) fn subtile_entry(&self, req: &TileRequest) -> Result<&TileEntry, WsiError> {
        let (_, _, level) = self
            .layout
            .dataset
            .level(req.scene, req.series, req.level)?;
        let TileLayout::Irregular { tiles, .. } = &level.tile_layout else {
            return Err(WsiError::UnsupportedFormat(
                "Ventana subtile levels must use irregular tilemaps".into(),
            ));
        };
        tiles.get(&(req.col, req.row)).ok_or_else(|| {
            WsiError::tile_read(
                req,
                format!("no Ventana subtile at ({},{})", req.col, req.row),
            )
        })
    }

    pub(super) fn stored_tile_geometry(&self, ifd_id: IfdId) -> Result<StoredTileGeometry, String> {
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

pub(super) struct StoredTileGeometry {
    pub(super) width: u64,
    pub(super) height: u64,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
    pub(super) tiles_across: u64,
    pub(super) tiles_down: u64,
}
