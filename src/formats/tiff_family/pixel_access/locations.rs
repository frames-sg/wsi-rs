//! Tile locations: which byte ranges of the TIFF file hold each tile, read
//! from the parsed IFDs without touching tile bodies.
//!
//! Every tile of one plane source shares its codec description, JPEG tables
//! and NDPI header bytes. They are built on a source's first location and
//! kept in [`TiffLocations`], so a level enumeration costs one table lookup
//! per tile, plus a small read of each JPEG tile's markers where the tags
//! leave its color to them (see [`TiffPixelReader::unit_codec`]).

use std::path::Path;

use super::stripped_level::StrippedLevelSource;
use super::*;

mod codec;
mod ndpi;
mod strips;

pub(super) use codec::{
    bare_codec_is_self_describing, bare_payload_is_self_describing, ycbcr_is_subsampled, UnitCodec,
};
use ndpi::NdpiParams;

/// The slide file every unit refers to, and the per-source parameters
/// shared by its tile locations.
pub(super) struct TiffLocations {
    path: Arc<Path>,
    file_len: u64,
    sources: Mutex<HashMap<TileSourceKey, Arc<SourceParams>>>,
}

impl TiffLocations {
    pub(super) fn new(container: &TiffContainer) -> Self {
        Self {
            path: Arc::from(container.path()),
            file_len: container.file_len(),
            sources: Mutex::new(HashMap::new()),
        }
    }

    /// One `framing` unit of `codec` holding `range`, validated against the
    /// file.
    fn unit(
        &self,
        (codec, framing): (EncodedCodec, StoredFraming),
        stored: (u32, u32),
        range: ByteRange,
        source_region: PixelRect,
        tile_region: PixelRect,
    ) -> Result<EncodedUnit, WsiError> {
        EncodedUnit::builder(self.path.clone(), codec, stored.0, stored.1)
            .segment(range)
            .framing(framing)
            .source_region(source_region)
            .tile_region(tile_region)
            .build(self.file_len)
    }
}

/// What every tile of one plane source shares.
enum SourceParams {
    /// Stored TIFF tiles or strips.
    Units(UnitCodec),
    /// NDPI restart intervals and the whole-level JPEG they belong to.
    Ndpi(NdpiParams),
}

impl TiffPixelReader {
    /// Where `req`'s encoded bytes are stored; see
    /// [`SlideReader::tile_location`].
    pub(super) fn tiff_tile_location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        let source = self.tile_source_for(req)?;
        match source {
            TileSource::TiledIfd { ifd_id, .. } => {
                let codec = self.location_params(req, source)?;
                let (tile_idx, width, height) =
                    self.tiled_ifd_tile_index_and_dimensions(req, *ifd_id)?;
                let region = PixelRect::new(0, 0, width, height);
                self.stored_tile_location(req, *ifd_id, &codec, tile_idx as u64, region)
            }
            TileSource::TiledIfdSubtiles {
                ifd_id,
                subtiles_per_tile,
                ..
            } => {
                let codec = self.location_params(req, source)?;
                self.subtile_location(req, *ifd_id, *subtiles_per_tile, &codec)
            }
            TileSource::StrippedLevel {
                ifd_id,
                compression,
                jpeg_tables,
                rows_per_strip,
                strip_offsets,
                strip_byte_counts,
            } => {
                let codec = self.location_params(req, source)?;
                self.stripped_level_location(
                    req,
                    &StrippedLevelSource {
                        ifd_id: *ifd_id,
                        compression: *compression,
                        jpeg_tables: jpeg_tables.as_deref(),
                        rows_per_strip: *rows_per_strip,
                        strip_offsets,
                        strip_byte_counts,
                    },
                    &codec,
                )
            }
            TileSource::NdpiJpeg { .. } | TileSource::NdpiFullDecode { .. } => {
                let params = self.location_params(req, source)?;
                let SourceParams::Ndpi(params) = params.as_ref() else {
                    return Err(WsiError::tile_read(req, "NDPI source has tile parameters"));
                };
                self.ndpi_tile_location(req, source, params)
            }
            TileSource::SyntheticDownsample { base_level, factor } => Ok(
                TileLocation::NotAddressable(NotAddressableReason::SyntheticLevel {
                    base_level: LevelIdx::new(*base_level),
                    factor: *factor,
                }),
            ),
            TileSource::Stripped { .. } | TileSource::ExternalJpeg { .. } => {
                Err(WsiError::tile_read(
                    req,
                    "tile locations are only available for pyramid levels, not associated images",
                ))
            }
        }
    }

    /// The cached parameters of `req`'s plane source, built on first use.
    /// Failures are not cached, so a transient read error is retried.
    fn location_params(
        &self,
        req: &TileRequest,
        source: &TileSource,
    ) -> Result<Arc<SourceParams>, WsiError> {
        let key = TileSourceKey::from(req);
        if let Some(params) = self.cached_location_params(&key) {
            return Ok(params);
        }
        let built = Arc::new(match source {
            TileSource::TiledIfd {
                ifd_id,
                jpeg_tables,
                compression,
            }
            | TileSource::TiledIfdSubtiles {
                ifd_id,
                jpeg_tables,
                compression,
                ..
            }
            | TileSource::StrippedLevel {
                ifd_id,
                jpeg_tables,
                compression,
                ..
            } => SourceParams::Units(self.tiff_unit_codec(
                *ifd_id,
                *compression,
                jpeg_tables.as_deref(),
            )?),
            TileSource::NdpiJpeg { .. } | TileSource::NdpiFullDecode { .. } => {
                SourceParams::Ndpi(self.ndpi_location_params(req, source)?)
            }
            _ => return Err(WsiError::tile_read(req, "source has no tile locations")),
        });
        Ok(self
            .locations
            .sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key)
            .or_insert(built)
            .clone())
    }

    fn cached_location_params(&self, key: &TileSourceKey) -> Option<Arc<SourceParams>> {
        self.locations
            .sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned()
    }

    /// The units of stored TIFF tile `tile_idx` (one per sample plane), or
    /// `Absent` when the file stores no bytes for it. `source_region` is the
    /// part of the stored tile the requested tile shows.
    fn stored_tile_location(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        params: &SourceParams,
        tile_idx: u64,
        source_region: PixelRect,
    ) -> Result<TileLocation, WsiError> {
        let SourceParams::Units(codec) = params else {
            return Err(WsiError::tile_read(
                req,
                "TIFF tile source has NDPI parameters",
            ));
        };
        let tiled = codec.tiled.ok_or_else(|| {
            WsiError::tile_read(req, "TIFF tile location needs TileWidth and TileLength")
        })?;
        let (offsets, byte_counts) = self.tiled_ifd_offsets_and_byte_counts(ifd_id)?;
        let tile_region = PixelRect::new(0, 0, source_region.width, source_region.height);
        let mut units = Vec::with_capacity(codec.planes as usize);
        let mut empty_planes = 0u64;
        for plane in 0..codec.planes {
            let index = plane
                .checked_mul(tiled.tiles_per_plane)
                .and_then(|first| first.checked_add(tile_idx))
                .and_then(|index| usize::try_from(index).ok());
            let entry = index.and_then(|index| offsets.get(index).zip(byte_counts.get(index)));
            let Some((&offset, &byte_count)) = entry else {
                return Err(WsiError::tile_read(
                    req,
                    format!(
                        "tile index {tile_idx} (sample plane {plane}) out of range \
                         (offsets={}, byte_counts={})",
                        offsets.len(),
                        byte_counts.len()
                    ),
                ));
            };
            if byte_count == 0 {
                empty_planes += 1;
                continue;
            }
            units.push(self.stored_unit(
                req,
                codec,
                (tiled.tile_width, tiled.tile_height),
                ByteRange::new(offset, byte_count),
                source_region,
                tile_region,
            )?);
        }
        if units.is_empty() {
            return Ok(TileLocation::Absent);
        }
        if empty_planes > 0 {
            return Err(WsiError::tile_read(
                req,
                format!("planar TIFF tile stores {empty_planes} of its sample planes as empty"),
            ));
        }
        Ok(TileLocation::Stored(StoredTile::new(
            source_region.width,
            source_region.height,
            units,
        )?))
    }

    /// One unit of `codec` holding `range`, with the codec of its own color
    /// markers where the tags leave its color to them. A range outside the
    /// file is refused by the unit builder before any read.
    fn stored_unit(
        &self,
        req: &TileRequest,
        codec: &UnitCodec,
        stored: (u32, u32),
        range: ByteRange,
        source_region: PixelRect,
        tile_region: PixelRect,
    ) -> Result<EncodedUnit, WsiError> {
        let in_file = range
            .end()
            .is_some_and(|end| end <= self.locations.file_len);
        let unit_codec = if in_file {
            self.unit_codec(req, codec, range)?
        } else {
            codec.codec.clone()
        };
        self.locations.unit(
            (unit_codec, codec.framing),
            stored,
            range,
            source_region,
            tile_region,
        )
    }

    /// A Ventana reduced-level cell: the `1 / subtiles_per_tile` share of a
    /// stored tile, as [`Self::read_tiled_ifd_subtile`] crops it.
    fn subtile_location(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        subtiles_per_tile: u32,
        params: &SourceParams,
    ) -> Result<TileLocation, WsiError> {
        let tile_error = |reason: String| WsiError::tile_read(req, reason);
        let entry = self.subtile_entry(req)?;
        if entry.has_explicit_extent() {
            // Fractional subtile sizes are painted by bilinear resampling.
            return Ok(TileLocation::NotAddressable(
                NotAddressableReason::Resampled,
            ));
        }
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
        let clipped = (
            (geometry.width - tile_col * u64::from(geometry.tile_width))
                .min(u64::from(geometry.tile_width)),
            (geometry.height - tile_row * u64::from(geometry.tile_height))
                .min(u64::from(geometry.tile_height)),
        );
        // Without an explicit extent the share is a whole number of pixels.
        let origin = (
            (col % per_tile) * u64::from(geometry.tile_width) / per_tile,
            (row % per_tile) * u64::from(geometry.tile_height) / per_tile,
        );
        if origin.0 >= clipped.0 || origin.1 >= clipped.1 {
            return Err(tile_error(format!(
                "Ventana subtile origin ({},{}) is outside the {}x{} stored tile",
                origin.0, origin.1, clipped.0, clipped.1
            )));
        }
        let source_region = PixelRect::new(
            origin.0 as u32,
            origin.1 as u32,
            u64::from(entry.dimensions.0).min(clipped.0 - origin.0) as u32,
            u64::from(entry.dimensions.1).min(clipped.1 - origin.1) as u32,
        );
        self.stored_tile_location(
            req,
            ifd_id,
            params,
            tile_row * geometry.tiles_across + tile_col,
            source_region,
        )
    }
}
