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
use crate::decode::jpeg::frame::{next_jpeg_segment, scan_baseline_jpeg_frame};

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

/// Leading bytes of a stored JPEG or J2K stream read to find its size:
/// the headers before the first scan or tile-part, which real tiles keep
/// within the first bound. A second, larger read covers long headers.
const STREAM_HEADER_READ_BYTES: [u64; 2] = [4 * 1024, 64 * 1024];

/// The (width, height) raw reads take from a JPEG, from its leading bytes:
/// the raw scanner stops at the first scan, so the headers decide it. A
/// `head` that is not the `complete` stream decides only once it reaches
/// that scan; before it, a later SOF could still replace the size.
fn jpeg_header_size(head: &[u8], complete: bool) -> Option<(u32, u32)> {
    if !head.starts_with(&[0xFF, 0xD8]) || !(complete || reaches_first_scan(head)) {
        return None;
    }
    scan_baseline_jpeg_frame(head)
        .ok()
        .map(|(_, _, info)| (info.width, info.height))
}

/// Whether the JPEG segments in `head` reach a whole SOS header.
fn reaches_first_scan(head: &[u8]) -> bool {
    let mut offset = 0;
    while let Ok(Some(segment)) = next_jpeg_segment(head, offset) {
        if segment.marker == 0xDA {
            return true;
        }
        offset = segment.end;
    }
    false
}

/// The (width, height) raw reads take from a J2K stream or JP2/JPH file,
/// from its leading bytes. JPEG 2000 inspection needs only the main header,
/// but in a file it needs whole boxes, so a partial file is inspected
/// through the start of its codestream box.
fn j2k_header_size(head: &[u8], complete: bool) -> Option<(u32, u32)> {
    let input = if complete {
        head
    } else {
        jp2_codestream_start(head).unwrap_or(head)
    };
    j2k::J2kDecoder::inspect(input)
        .ok()
        .map(|info| info.dimensions)
}

/// The payload of the first `jp2c` box in a JP2/JPH file prefix, as far as
/// `head` holds it; `None` for a bare codestream or a box not reached.
fn jp2_codestream_start(head: &[u8]) -> Option<&[u8]> {
    const SIGNATURE: [u8; 8] = [0, 0, 0, 12, b'j', b'P', b' ', b' '];
    if !head.starts_with(&SIGNATURE) {
        return None;
    }
    let mut at = 0usize;
    loop {
        let header = head.get(at..at.checked_add(8)?)?;
        let declared = u32::from_be_bytes(header[..4].try_into().ok()?);
        let (header_len, box_len) = match declared {
            1 => {
                let extended = head.get(at + 8..at + 16)?;
                (16, u64::from_be_bytes(extended.try_into().ok()?))
            }
            0 => (8, u64::MAX),
            len => (8, u64::from(len)),
        };
        if box_len < header_len as u64 {
            return None;
        }
        let box_len = usize::try_from(box_len).unwrap_or(usize::MAX);
        if &header[4..] == b"jp2c" {
            let end = at.saturating_add(box_len).min(head.len());
            return head.get(at + header_len..end);
        }
        at = at.checked_add(box_len)?;
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
                self.stored_tile_location(
                    req,
                    *ifd_id,
                    &codec,
                    tile_idx as u64,
                    region,
                    (width, height),
                )
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
    /// part of the stored tile the requested tile shows, and `clipped` the
    /// stored tile's size inside the image.
    fn stored_tile_location(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        params: &SourceParams,
        tile_idx: u64,
        source_region: PixelRect,
        clipped: (u32, u32),
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
            let range = ByteRange::new(offset, byte_count);
            let tile = (tiled.tile_width, tiled.tile_height);
            let stored = self.stored_tile_size(codec, tile, clipped, range)?;
            units.push(self.stored_unit(req, codec, stored, range, source_region, tile_region)?);
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

    /// The size a stored tile at `range` is encoded at. TIFF stores every
    /// tile at the full `tile` size, but some writers encode JPEG and J2K
    /// tiles clipped by the right or bottom edge at their `clipped` size, or
    /// larger. Raw reads report the size in the stream's own header, so a
    /// clipped tile's headers are read and parsed here with the same
    /// functions; interior tiles add no reads. A stream whose size cannot be
    /// read, or that raw reads would refuse as too small, keeps `tile`.
    fn stored_tile_size(
        &self,
        codec: &UnitCodec,
        tile: (u32, u32),
        clipped: (u32, u32),
        range: ByteRange,
    ) -> Result<(u32, u32), WsiError> {
        let compression = codec.codec.compression();
        let has_header = matches!(
            compression,
            Compression::Jpeg | Compression::Jp2kRgb | Compression::Jp2kYcbcr
        );
        let in_file = range
            .end()
            .is_some_and(|end| end <= self.locations.file_len);
        if clipped == tile || !has_header || !in_file {
            return Ok(tile);
        }
        for limit in STREAM_HEADER_READ_BYTES {
            let len = range.length.min(limit);
            let head = self
                .container
                .pread(range.offset, len)
                .map_err(|err| err.into_wsi_error(self.container.path()))?;
            let complete = len == range.length;
            let size = if compression == Compression::Jpeg {
                jpeg_header_size(&head, complete)
            } else {
                j2k_header_size(&head, complete)
            };
            if size.is_some() || complete {
                // Raw reads refuse a stream smaller than the clipped tile.
                return Ok(size
                    .filter(|&(width, height)| width >= clipped.0 && height >= clipped.1)
                    .unwrap_or(tile));
            }
        }
        Ok(tile)
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
        let cell = self.subtile_cell(req, ifd_id, subtiles_per_tile)?;
        let geometry = &cell.geometry;
        let per_tile = cell.per_tile;
        let (col, row) = cell.cell;
        let clipped = cell.clipped;
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
        let clipped_tile = (clipped.0 as u32, clipped.1 as u32);
        self.stored_tile_location(
            req,
            ifd_id,
            params,
            cell.tile_index(),
            source_region,
            clipped_tile,
        )
    }
}
