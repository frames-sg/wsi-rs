//! Raw JPEG tiles cut losslessly from larger stored JPEGs: Ventana subtiles,
//! strip-level tiles and NDPI whole-level JPEGs.

use super::*;
use crate::decode::jpeg::frame::{
    merge_missing_jpeg_tables, next_jpeg_segment, parse_sof0_frame_info, JpegFrameInfo,
};
use crate::decode::jpeg::is_sof_marker;
use crate::decode::jpeg::lossless::{
    lossless_crop_coefficient_budget, lossless_crop_jpeg_stack, lossless_crop_or_whole_jpeg,
    lossless_crop_output_bound,
};

impl TiffPixelReader {
    /// Crops `rect` out of `parts`, standalone JPEGs stacked top to bottom,
    /// without changing quantized coefficients. A rectangle covering a whole
    /// single part returns that part unchanged, whatever its frame type
    /// (baseline, extended or progressive).
    ///
    /// The sources, the coefficient planes and the worst-case output share
    /// the lossless-crop limit: the coefficient budget is what remains of it
    /// after the sources and the output ([`lossless_crop_coefficient_budget`]).
    /// A single part whose crop needs more is returned whole, with
    /// `tile_region` locating `rect` in it; stacked parts fail with
    /// [`WsiError::ResourceLimit`]. An output above the encoded-unit limit
    /// fails too. `photometric` maps the output and its frame description to
    /// the photometric interpretation the source decodes with.
    pub(super) fn lossless_jpeg_raw_tile(
        &self,
        mut parts: Vec<Vec<u8>>,
        rect: PixelRect,
        photometric: impl FnOnce(
            &[u8],
            &JpegFrameInfo,
        ) -> Result<EncodedTilePhotometricInterpretation, WsiError>,
    ) -> Result<RawCompressedTile, WsiError> {
        let whole_part = match parts.as_slice() {
            [part] if rect.x == 0 && rect.y == 0 => jpeg_frame_info(part)
                .filter(|info| (info.width, info.height) == (rect.width, rect.height)),
            _ => None,
        };
        let (data, info, tile_region) = match whole_part {
            Some(info) => (parts.swap_remove(0), info, None),
            None => {
                let source_bytes = parts
                    .iter()
                    .fold(0u64, |total, part| total.saturating_add(part.len() as u64));
                let budget = lossless_crop_coefficient_budget(
                    self.container.limits().lossless_crop_bytes(),
                    source_bytes,
                    (rect.width, rect.height),
                );
                let tuple = (rect.x, rect.y, rect.width, rect.height);
                let crop = if parts.len() == 1 {
                    lossless_crop_or_whole_jpeg(
                        parts.swap_remove(0),
                        tuple,
                        budget,
                        self.container.limits().encoded_unit_bytes(),
                    )?
                } else {
                    let parts: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
                    lossless_crop_jpeg_stack(&parts, tuple, budget)?
                };
                let (x, y, width, height) = crop.tile_region;
                if (width, height) != (rect.width, rect.height) {
                    return Err(WsiError::Unsupported {
                        reason: format!(
                            "JPEG crop covers {width}x{height}, not the {}x{} tile; the stored \
                             JPEG is smaller than its container declares",
                            rect.width, rect.height
                        ),
                    });
                }
                let region = PixelRect::new(x, y, width, height);
                let whole = PixelRect::new(0, 0, crop.width, crop.height);
                (crop.data, crop.info, (region != whole).then_some(region))
            }
        };
        self.check_raw_output_limit(data.len(), "lossless JPEG crop output")?;
        let photometric_interpretation = photometric(&data, &info)?;
        let builder = RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(info.width, info.height)
            .bits_allocated(info.bits_allocated)
            .samples_per_pixel(info.samples_per_pixel)
            .photometric_interpretation(photometric_interpretation)
            .data(data);
        Ok(match tile_region {
            Some(region) => builder.tile_region(region),
            None => builder,
        }
        .build()?)
    }

    /// A raw JPEG of the `rect` of TIFF JPEG `units`, one stored tile or
    /// strips stacked top to bottom, with `jpeg_tables` merged into each.
    /// Progressive units are accepted; the crop validates every frame.
    pub(super) fn tiff_jpeg_units_raw_tile(
        &self,
        ifd_id: IfdId,
        units: &[EncodedUnit],
        rect: PixelRect,
        jpeg_tables: Option<&[u8]>,
    ) -> Result<RawCompressedTile, WsiError> {
        let merged_tables = jpeg_tables.map_or(0, |tables| tables.len() as u64);
        self.check_lossless_crop_sources(units, rect, merged_tables)?;
        let parts = units
            .iter()
            .map(|unit| merge_missing_jpeg_tables(self.read_single_segment(unit)?, jpeg_tables))
            .collect::<Result<Vec<_>, WsiError>>()?;
        self.lossless_jpeg_raw_tile(parts, rect, |data, info| {
            self.tiff_jpeg_raw_photometric(ifd_id, data, info)
        })
    }

    /// Refuses, from the location metadata alone and before any unit is
    /// read, a crop that cannot be served. Each unit is prepared as its
    /// stored bytes, its framing and up to `merged_tables` bytes of JPEG
    /// tables merged in.
    ///
    /// - One unit must make a standalone JPEG within the encoded-unit limit.
    ///   A crop that then does not fit the lossless-crop limit returns that
    ///   whole JPEG, so nothing more is refused.
    /// - Stacked units have no single JPEG to return instead: the prepared
    ///   streams and the crop's worst-case output must fit the lossless-crop
    ///   limit (the coefficient budget is what then remains of it).
    fn check_lossless_crop_sources(
        &self,
        units: &[EncodedUnit],
        rect: PixelRect,
        merged_tables: u64,
    ) -> Result<(), WsiError> {
        let limits = self.container.limits();
        let (prepared, _) = crop_source_preparation(units, merged_tables);
        let (requested, limit, resource) = match units {
            [_] => (
                prepared,
                limits.encoded_unit_bytes(),
                "encoded tile/frame unit",
            ),
            _ => (
                prepared.saturating_add(lossless_crop_output_bound(
                    prepared,
                    rect.width,
                    rect.height,
                )),
                limits.lossless_crop_bytes(),
                "lossless JPEG crop sources and output",
            ),
        };
        if requested > limit {
            return Err(WsiError::ResourceLimit {
                resource,
                requested,
                limit,
            });
        }
        Ok(())
    }

    /// Transient bytes a raw read of `req` reserves when it returns stored
    /// JPEG units of a subtile, strip or NDPI level, or `None` for other
    /// reads: preparing the units' standalone streams
    /// ([`crop_source_preparation`]), raised to the lossless-crop limit when
    /// the read crops them. Either covers the returned tile, a crop or a
    /// whole stored JPEG.
    pub(super) fn raw_jpeg_unit_reservation(
        &self,
        req: &TileRequest,
    ) -> Result<Option<u64>, WsiError> {
        let (location, merged_tables) = match self.tile_source_for(req)? {
            // Only tiles no restart interval serves read the level JPEG. A
            // level JPEG that cannot be located leaves the restart read's
            // result in place, so it is not read either.
            TileSource::NdpiJpeg { .. } => match self.ndpi_level_jpeg_location(req) {
                Ok(Some(location)) => (location, 0),
                Ok(None) | Err(_) => return Ok(None),
            },
            TileSource::NdpiFullDecode { .. } => (self.tiff_tile_location(req)?, 0),
            TileSource::TiledIfdSubtiles {
                compression: Compression::Jpeg,
                jpeg_tables,
                ..
            }
            | TileSource::StrippedLevel {
                compression: Compression::Jpeg,
                jpeg_tables,
                ..
            } => (
                self.tiff_tile_location(req)?,
                jpeg_tables.as_ref().map_or(0, |tables| tables.len() as u64),
            ),
            _ => return Ok(None),
        };
        let TileLocation::Stored(stored) = location else {
            return Ok(None);
        };
        let (prepared, preparation) = crop_source_preparation(stored.units(), merged_tables);
        let preparing = prepared.saturating_add(preparation);
        let crops = match stored.units() {
            [unit] => {
                let (width, height) = unit.stored_size();
                unit.source_region() != PixelRect::new(0, 0, width, height)
            }
            _ => true,
        };
        Ok(Some(if crops {
            self.container.limits().lossless_crop_bytes().max(preparing)
        } else {
            preparing
        }))
    }

    /// Fails when a raw tile built for a source admitted at the encoded-unit
    /// limit outgrows it.
    pub(super) fn check_raw_output_limit(
        &self,
        len: usize,
        resource: &'static str,
    ) -> Result<(), WsiError> {
        let limit = self.container.limits().encoded_unit_bytes();
        let requested = len as u64;
        if requested > limit {
            return Err(WsiError::ResourceLimit {
                resource,
                requested,
                limit,
            });
        }
        Ok(())
    }

    /// The bytes of `unit`'s single segment, refused above the encoded-unit
    /// limit before reading.
    pub(super) fn read_single_segment(&self, unit: &EncodedUnit) -> Result<Vec<u8>, WsiError> {
        let [segment] = unit.segments() else {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "expected one stored segment, the unit has {}",
                    unit.segments().len()
                ),
            });
        };
        let limit = self.container.limits().encoded_unit_bytes();
        if segment.length > limit {
            return Err(WsiError::ResourceLimit {
                resource: "encoded tile/frame unit",
                requested: segment.length,
                limit,
            });
        }
        self.container
            .pread(segment.offset, segment.length)
            .map_err(|err| err.into_wsi_error(self.container.path()))
    }

    /// Raw passthrough of an NDPI tile at `location` in its level's single
    /// JPEG: the JPEG itself when the tile is the whole level, otherwise a
    /// lossless crop of the tile's rectangle. Levels without restart markers
    /// are read this way, and so are restart-interval tiles the MCU-starts
    /// table cannot locate or whose intervals cross image rows. A level too
    /// large to crop within the lossless-crop limit returns its whole JPEG,
    /// with `tile_region` locating the tile; one over the encoded-unit limit
    /// fails with [`WsiError::ResourceLimit`].
    pub(super) fn read_ndpi_level_jpeg_raw_tile(
        &self,
        req: &TileRequest,
        location: TileLocation,
    ) -> Result<RawCompressedTile, WsiError> {
        let unit = match location {
            TileLocation::Stored(stored) => stored
                .single_unit()
                .cloned()
                .ok_or_else(|| WsiError::tile_read(req, "NDPI level JPEG has several units"))?,
            TileLocation::NotAddressable(_) => {
                return Err(WsiError::Unsupported {
                    reason: "the NDPI level JPEG states no frame size and the level exceeds the \
                             65535-pixel JPEG frame limit, so no standard JPEG holds its tiles"
                        .into(),
                })
            }
            TileLocation::Absent => {
                return Err(WsiError::tile_read(req, "NDPI level stores no JPEG"));
            }
        };
        let ifd_id = match self.tile_source_for(req)? {
            TileSource::NdpiJpeg { ifd_id, .. } | TileSource::NdpiFullDecode { ifd_id, .. } => {
                *ifd_id
            }
            _ => return Err(WsiError::tile_read(req, "source is not an NDPI JPEG level")),
        };
        self.check_lossless_crop_sources(std::slice::from_ref(&unit), unit.source_region(), 0)?;
        let stream = framed_stream(&unit, self.read_single_segment(&unit)?)?;
        self.lossless_jpeg_raw_tile(vec![stream], unit.source_region(), |data, info| {
            self.tiff_jpeg_raw_photometric(ifd_id, data, info)
        })
    }
}

/// Bytes of preparing `units` as standalone streams, each its stored bytes,
/// its framing and up to `merged_tables` bytes of JPEG tables: the prepared
/// streams, and the peak preparing the last one adds beside them, its stored
/// bytes and a copy of the tables (units read as stored, with no tables or
/// framing to add, are not copied).
fn crop_source_preparation(units: &[EncodedUnit], merged_tables: u64) -> (u64, u64) {
    let stored = |unit: &EncodedUnit| unit.stored_len().unwrap_or(u64::MAX);
    let prepared = units.iter().fold(0u64, |total, unit| {
        let framed = stored(unit)
            .saturating_add((unit.prefix().len() + unit.suffix().len()) as u64)
            .saturating_add(merged_tables);
        total.saturating_add(framed)
    });
    let preparation = units
        .iter()
        .filter(|unit| merged_tables > 0 || unit.framing() != StoredFraming::Standalone)
        .map(|unit| stored(unit).saturating_add(merged_tables))
        .max()
        .unwrap_or(0);
    (prepared, preparation)
}

/// The standalone stream of `unit` from its stored `bytes`, which are
/// released once a framed copy is built.
fn framed_stream(unit: &EncodedUnit, bytes: Vec<u8>) -> Result<Vec<u8>, WsiError> {
    if unit.framing() == StoredFraming::Standalone {
        return Ok(bytes);
    }
    unit.assemble([bytes.as_slice()])
}

/// The frame of `data`'s first SOF marker of any frame type, when it states
/// a nonzero 8-bit size with one or three components.
fn jpeg_frame_info(data: &[u8]) -> Option<JpegFrameInfo> {
    let mut offset = 0;
    while let Some(segment) = next_jpeg_segment(data, offset).ok()? {
        match segment.marker {
            0xDA => return None,
            marker if is_sof_marker(marker) => return parse_sof0_frame_info(segment.payload).ok(),
            _ => offset = segment.end,
        }
    }
    None
}
