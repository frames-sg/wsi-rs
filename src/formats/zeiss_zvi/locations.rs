//! Where ZVI plane payloads are stored, and their raw passthrough.
//!
//! Each plane is one compound-file stream: an image header, then the
//! payload at `payload_offset`. The payload is uncompressed little-endian
//! grayscale samples, a zlib stream after the `WZL` header, or a JPEG.

use super::model::{RawReadWindow, ZviCompression, ZviPlane, ZviSlide, ZviTileSource};
use super::sectors::CfbSectorMap;
use super::*;
use crate::decode::jpeg::lossless::{
    lossless_crop_coefficient_budget, lossless_crop_or_whole_jpeg,
};

impl ZviSlide {
    /// Where the payload of the plane that holds `req` is stored. A tile
    /// that is a window of its plane uses that window of the whole payload.
    pub(super) fn tile_location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        let ZviTileSource::Plane { index, window } = self.tile_source(req)? else {
            return Ok(TileLocation::Absent);
        };
        let plane = &self.planes[index];
        let segments = self.payload_segments()?[index].clone();
        let bits = plane_bits(plane)?;
        let (compression, framing) = match plane.compression {
            ZviCompression::Raw => (Compression::None, StoredFraming::BarePayload),
            ZviCompression::Zlib => (Compression::Deflate, StoredFraming::BarePayload),
            ZviCompression::Jpeg => (Compression::Jpeg, StoredFraming::Standalone),
        };
        let mut codec = EncodedCodec::new(
            compression,
            bits,
            1,
            EncodedTilePhotometricInterpretation::Monochrome2,
        );
        if bits > 8 && plane.compression != ZviCompression::Jpeg {
            codec = codec.with_byte_order(ByteOrder::LittleEndian);
        }
        let unit = EncodedUnit::builder(self.source_path.clone(), codec, plane.width, plane.height)
            .segments(segments)
            .framing(framing)
            .source_region(window)
            .tile_region(PixelRect::new(0, 0, window.width, window.height))
            .build(self.source_identity.length())?;
        Ok(TileLocation::Stored(StoredTile::new(
            window.width,
            window.height,
            vec![unit],
        )?))
    }

    /// The tile's stored payload or exact samples; see
    /// `ZviReader::read_raw_compressed_tile`.
    pub(super) fn read_raw_compressed_tile(
        &self,
        req: &TileRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        let (index, window) = match self.tile_source(req)? {
            ZviTileSource::Plane { index, window } => (index, window),
            ZviTileSource::Missing(reason) => {
                return Err(WsiError::Unsupported {
                    reason: format!(
                        "{reason}: ZVI tile ({}, {}) has no stored pixels (tile_location reports it Absent)",
                        req.col, req.row
                    ),
                })
            }
        };
        let plane = &self.planes[index];
        let bits = plane_bits(plane)?;
        let whole_plane = window == PixelRect::new(0, 0, plane.width, plane.height);
        let tile = |compression, width, height| {
            RawCompressedTile::builder(compression)
                .dimensions(width, height)
                .bits_allocated(bits)
                .samples_per_pixel(1)
                .photometric_interpretation(EncodedTilePhotometricInterpretation::Monochrome2)
        };
        let built = match (plane.compression, whole_plane) {
            (ZviCompression::Raw, true) => tile(Compression::None, plane.width, plane.height)
                .data(host_order_samples(self.read_plane_payload(plane)?, bits)),
            (ZviCompression::Zlib, true) => tile(Compression::Deflate, plane.width, plane.height)
                .data(self.read_plane_payload(plane)?),
            (ZviCompression::Jpeg, true) => tile(Compression::Jpeg, plane.width, plane.height)
                .data(self.read_plane_payload(plane)?),
            (ZviCompression::Jpeg, false) => {
                // The payload, the coefficient planes and the worst-case
                // output share the crop working set admission reserved; a
                // crop that needs more returns the whole payload.
                let payload = self.read_plane_payload(plane)?;
                let budget = lossless_crop_coefficient_budget(
                    self.lossless_crop_bytes,
                    payload.len() as u64,
                    (window.width, window.height),
                );
                let crop = lossless_crop_or_whole_jpeg(
                    payload,
                    (window.x, window.y, window.width, window.height),
                    budget,
                    self.encoded_unit_bytes,
                )?;
                return crop.into_raw_tile((window.width, window.height));
            }
            (ZviCompression::Raw | ZviCompression::Zlib, false) => {
                let samples = self.plane_window_bytes(
                    plane,
                    RawReadWindow {
                        x: window.x,
                        y: window.y,
                        width: window.width,
                        height: window.height,
                        bytes_per_sample: u64::from(plane.bytes_per_sample),
                    },
                )?;
                tile(Compression::None, window.width, window.height)
                    .data(host_order_samples(samples, bits))
            }
        };
        Ok(built.build()?)
    }

    /// [`SlideReader::raw_compressed_tile_reservation_hint`] for `req`: the
    /// lossless-crop limit when the tile is a crop of a JPEG plane,
    /// otherwise 0, leaving one encoded unit.
    pub(super) fn raw_tile_reservation_hint(&self, req: &TileRequest) -> Result<u64, WsiError> {
        let ZviTileSource::Plane { index, window } = self.tile_source(req)? else {
            return Ok(0);
        };
        let plane = &self.planes[index];
        let whole_plane = window == PixelRect::new(0, 0, plane.width, plane.height);
        Ok(
            if plane.compression == ZviCompression::Jpeg && !whole_plane {
                self.lossless_crop_bytes
            } else {
                0
            },
        )
    }

    /// File ranges of every plane's payload, mapped once from the compound
    /// file's tables. A failed mapping is not kept, so a later call retries.
    fn payload_segments(&self) -> Result<&[Vec<ByteRange>], WsiError> {
        if let Some(segments) = self.payload_segments.get() {
            return Ok(segments);
        }
        let segments = self.map_payload_segments()?;
        Ok(self.payload_segments.get_or_init(|| segments))
    }

    fn map_payload_segments(&self) -> Result<Vec<Vec<ByteRange>>, WsiError> {
        let file = FileIdentity::open_verified(
            &self.source_path,
            &self.source_identity,
            "ZVI source changed after it was opened",
        )?;
        let invalid = |reason: String| {
            WsiError::invalid_slide(
                &self.source_path,
                format!("ZVI payload locations: {reason}"),
            )
        };
        let mut map = CfbSectorMap::read(
            &file,
            self.source_identity.length(),
            MAX_ZVI_DIRECTORY_ENTRIES,
            self.location_index_limit,
        )
        .map_err(invalid)?;
        let range_bytes = std::mem::size_of::<ByteRange>() as u64;
        let mut retained = 0u64;
        let mut planes = Vec::new();
        planes
            .try_reserve_exact(self.planes.len())
            .map_err(|_| WsiError::ResourceLimit {
                resource: "ZVI payload location index",
                requested: self.planes.len() as u64,
                limit: self.location_index_limit,
            })?;
        for plane in &self.planes {
            let stream_len = plane
                .payload_offset
                .checked_add(plane.payload_length)
                .ok_or_else(|| invalid(format!("stream {} length overflows", plane.stream_path)))?;
            let ranges = map
                .stream_ranges(
                    &plane.stream_path,
                    stream_len,
                    plane.payload_offset,
                    plane.payload_length,
                )
                .map_err(invalid)?;
            retained =
                retained.saturating_add((ranges.len() as u64 + 2).saturating_mul(range_bytes));
            if retained > self.location_index_limit {
                return Err(WsiError::ResourceLimit {
                    resource: "ZVI payload location index",
                    requested: retained,
                    limit: self.location_index_limit,
                });
            }
            planes.push(ranges);
        }
        Ok(planes)
    }
}

/// Bits of one stored sample: 8 or 16.
fn plane_bits(plane: &ZviPlane) -> Result<u16, WsiError> {
    match plane.bytes_per_sample {
        1 => Ok(8),
        2 => Ok(16),
        other => Err(WsiError::Unsupported {
            reason: format!("unsupported ZVI sample byte depth {other}"),
        }),
    }
}

/// Little-endian stored samples in host byte order.
fn host_order_samples(mut samples: Vec<u8>, bits: u16) -> Vec<u8> {
    if bits == 16 && cfg!(target_endian = "big") {
        for word in samples.as_chunks_mut::<2>().0 {
            word.swap(0, 1);
        }
    }
    samples
}
