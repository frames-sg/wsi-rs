use super::super::locations::{bare_payload_is_self_describing, ycbcr_is_subsampled, UnitCodec};
use super::super::*;
use super::addressing::TiledIfdTileSpan;
use super::codecs::inflate_tiff_payload;

impl TiffPixelReader {
    /// Raw passthrough of a TIFF tile stored with a codec other than JPEG or
    /// J2K: JPEG XR, LZW, Deflate, Zstandard, uncompressed and other bare
    /// payloads.
    ///
    /// JPEG XR tiles, and bare payloads that [`RawCompressedTile`] fully
    /// describes (no predictor, chunky samples in this machine's byte order,
    /// YCbCr only without subsampling), are returned exactly as stored, which
    /// is what assembling the tile's location yields. A bare payload keeps
    /// the stored tile size, padding included; at a clipped edge
    /// [`RawCompressedTile::tile_region`] marks the tile inside it. Other
    /// lossless payloads (a predictor, big-endian samples, separate 8-bit RGB
    /// sample planes) are decoded and returned as `Compression::None` samples
    /// of the tile: interleaved, 8-bit or native-endian 16-bit. That loses
    /// nothing because the stored codec is lossless. Subsampled YCbCr
    /// payloads stay unsupported rather than being color-converted.
    pub(in super::super) fn read_tiled_ifd_raw_payload_tile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        compression: Compression,
    ) -> Result<RawCompressedTile, WsiError> {
        let span = self.tiled_ifd_tile_span(req, ifd_id)?;
        if span.byte_count == 0 {
            return Err(WsiError::Unsupported {
                reason: "compressed passthrough does not support empty TIFF tiles; \
                         tile_location reports them as Absent"
                    .into(),
            });
        }
        let unit = self.tiff_unit_codec(ifd_id, compression, None);
        if let Ok(unit) = &unit {
            if unit.planes > 1 {
                return self.read_planar_raw_tile(req, compression, unit);
            }
            if compression == Compression::JpegXr || bare_payload_is_self_describing(unit) {
                return self.stored_raw_payload_tile(span, compression, unit);
            }
            if ycbcr_is_subsampled(&unit.codec) {
                return Err(WsiError::Unsupported {
                    reason: "subsampled YCbCr TIFF payloads have no raw sample layout and are \
                             not color-converted; read their byte ranges through tile_location"
                        .into(),
                });
            }
        }
        if !is_lossless_tiff_codec(compression) {
            return Err(unit.err().unwrap_or_else(|| WsiError::Unsupported {
                reason: format!(
                    "{compression:?} TIFF tiles cannot be passed through as stored; read their \
                     byte ranges through tile_location"
                ),
            }));
        }

        let (tile_idx, width, height) = self.tiled_ifd_tile_index_and_dimensions(req, ifd_id)?;
        let (offsets, byte_counts) = self.tiled_ifd_offsets_and_byte_counts(ifd_id)?;
        let tile = self
            .decode_tiled_ifd_tile_index(
                ifd_id,
                tile_idx,
                None,
                compression,
                width,
                height,
                offsets,
                byte_counts,
                BackendRequest::Cpu,
            )
            .map_err(|err| match err {
                WsiError::TileRead { .. } => err,
                other => WsiError::tile_read(req, other.to_string()),
            })?;
        uncompressed_raw_tile(&tile)
    }

    fn stored_raw_payload_tile(
        &self,
        span: TiledIfdTileSpan,
        compression: Compression,
        unit: &UnitCodec,
    ) -> Result<RawCompressedTile, WsiError> {
        let data = self.read_tiled_ifd_tile_span(span)?;
        let codec = &unit.codec;
        let builder = RawCompressedTile::builder(compression)
            .bits_allocated(codec.bits_per_sample())
            .samples_per_pixel(codec.samples_per_pixel())
            .photometric_interpretation(codec.photometric_interpretation())
            .data(data);
        let builder = match unit.tiled {
            // A bare payload has no size of its own: report the stored tile.
            Some(tiled) if compression != Compression::JpegXr => {
                let builder = builder.dimensions(tiled.tile_width, tiled.tile_height);
                if (span.width, span.height) == (tiled.tile_width, tiled.tile_height) {
                    builder
                } else {
                    builder.tile_region(PixelRect::new(0, 0, span.width, span.height))
                }
            }
            _ => builder.dimensions(span.width, span.height),
        };
        Ok(builder.build()?)
    }

    /// A tile stored as separate 8-bit RGB sample planes: each plane is
    /// inflated, its horizontal predictor undone, on its own, and the planes
    /// are interleaved into the tile's exact samples.
    fn read_planar_raw_tile(
        &self,
        req: &TileRequest,
        compression: Compression,
        unit: &UnitCodec,
    ) -> Result<RawCompressedTile, WsiError> {
        let codec = &unit.codec;
        let tiled = unit.tiled.filter(|_| {
            is_lossless_tiff_codec(compression)
                && unit.planes == 3
                && codec.bits_per_sample() == 8
                && matches!(codec.predictor(), Predictor::None | Predictor::Horizontal)
                && codec.photometric_interpretation() == EncodedTilePhotometricInterpretation::Rgb
        });
        let Some(tiled) = tiled else {
            return Err(WsiError::Unsupported {
                reason: "planar TIFF tiles are returned as samples only for 8-bit RGB stored \
                         uncompressed or with LZW, Deflate or Zstandard, with no predictor or \
                         horizontal differencing; read the sample planes' byte ranges through \
                         tile_location"
                    .into(),
            });
        };
        let TileLocation::Stored(stored) = self.tiff_tile_location(req)? else {
            return Err(WsiError::tile_read(
                req,
                "planar TIFF tile stores no planes",
            ));
        };
        let plane_len = checked_product_to_usize(
            &[u64::from(tiled.tile_width), u64::from(tiled.tile_height)],
            MAX_DECODED_IMAGE_BYTES,
            "planar TIFF tile plane",
        )
        .map_err(|reason| WsiError::tile_read(req, reason))?;
        let planes = stored
            .units()
            .iter()
            .map(|plane| {
                let data = self.read_single_segment(plane)?;
                let mut samples = if compression == Compression::None {
                    if data.len() < plane_len {
                        return Err(WsiError::tile_read(
                            req,
                            format!(
                                "uncompressed TIFF sample plane holds {} bytes, expected \
                                 {plane_len}",
                                data.len()
                            ),
                        ));
                    }
                    data
                } else {
                    inflate_tiff_payload(compression, &data, plane_len)?
                };
                if codec.predictor() == Predictor::Horizontal {
                    // Each plane holds one sample per pixel.
                    for row in samples[..plane_len].chunks_exact_mut(tiled.tile_width as usize) {
                        for x in 1..row.len() {
                            row[x] = row[x].wrapping_add(row[x - 1]);
                        }
                    }
                }
                Ok(samples)
            })
            .collect::<Result<Vec<_>, WsiError>>()?;
        let (width, height) = stored.tile_size();
        let stride = tiled.tile_width as usize;
        let mut samples = Vec::with_capacity(width as usize * height as usize * planes.len());
        for y in 0..height as usize {
            for x in 0..width as usize {
                samples.extend(planes.iter().map(|plane| plane[y * stride + x]));
            }
        }
        Ok(RawCompressedTile::builder(Compression::None)
            .dimensions(width, height)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::Rgb)
            .data(samples)
            .build()?)
    }

    /// The bytes [`Self::read_tiled_ifd_raw_payload_tile`] holds at once for
    /// `req`: the stored payload when it is returned as stored, otherwise
    /// the compressed planes plus the physical decode buffers (a decoded
    /// stored tile and its sample copy) and the cropped tile and its raw
    /// copy.
    pub(in super::super) fn tiled_ifd_raw_payload_bound(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        compression: Compression,
        span: TiledIfdTileSpan,
    ) -> Result<u64, WsiError> {
        let unit = self.tiff_unit_codec(ifd_id, compression, None);
        let stored = unit.as_ref().is_ok_and(|unit| {
            unit.planes == 1
                && (compression == Compression::JpegXr || bare_payload_is_self_describing(unit))
        });
        if span.byte_count == 0 || stored {
            return Ok(span.byte_count);
        }
        let tag = |tag| self.container.get_u32(ifd_id, tag).ok();
        let samples = u64::from(tag(tags::SAMPLES_PER_PIXEL).unwrap_or(1));
        let bits = self
            .container
            .get_u64_array(ifd_id, tags::BITS_PER_SAMPLE)
            .ok()
            .and_then(|bits| bits.iter().copied().max())
            .unwrap_or(8);
        let sample_bytes = bits.div_ceil(8).max(1);
        let pixels = |width: u32, height: u32| u64::from(width).saturating_mul(u64::from(height));
        let physical = pixels(
            tag(tags::TILE_WIDTH).unwrap_or(span.width),
            tag(tags::TILE_LENGTH).unwrap_or(span.height),
        )
        .saturating_mul(samples)
        .saturating_mul(sample_bytes);
        let logical = pixels(span.width, span.height)
            .saturating_mul(samples)
            .saturating_mul(sample_bytes);
        match unit {
            Ok(unit) if unit.planes > 1 => {
                // Every plane's payload and inflated plane, then the samples.
                let (tile_idx, _, _) = self.tiled_ifd_tile_index_and_dimensions(req, ifd_id)?;
                let (_, byte_counts) = self.tiled_ifd_offsets_and_byte_counts(ifd_id)?;
                let per_plane = unit.tiled.map_or(0, |tiled| tiled.tiles_per_plane);
                let compressed = (0..unit.planes)
                    .filter_map(|plane| {
                        let index = plane.checked_mul(per_plane)?.checked_add(tile_idx as u64)?;
                        byte_counts.get(usize::try_from(index).ok()?).copied()
                    })
                    .fold(0u64, u64::saturating_add);
                Ok(compressed.saturating_add(physical).saturating_add(logical))
            }
            _ => Ok(span
                .byte_count
                .saturating_add(physical.saturating_mul(2))
                .saturating_add(logical.saturating_mul(2))),
        }
    }
}

/// Codecs whose TIFF payloads the reader decodes without loss.
pub(in super::super) fn is_lossless_tiff_codec(compression: Compression) -> bool {
    matches!(
        compression,
        Compression::None | Compression::Lzw | Compression::Deflate | Compression::Zstd
    )
}

/// Decoded samples as an uncompressed raw tile: interleaved, 8-bit or
/// native-endian 16-bit, grayscale, RGB or unsubsampled YCbCr.
pub(in super::super) fn uncompressed_raw_tile(
    tile: &CpuTile,
) -> Result<RawCompressedTile, WsiError> {
    let photometric = match (&tile.color_space, tile.channels, tile.layout) {
        (ColorSpace::Grayscale, 1, CpuTileLayout::Interleaved) => {
            EncodedTilePhotometricInterpretation::Monochrome2
        }
        (ColorSpace::Rgb, 3, CpuTileLayout::Interleaved) => {
            EncodedTilePhotometricInterpretation::Rgb
        }
        (ColorSpace::YCbCr, 3, CpuTileLayout::Interleaved) => {
            EncodedTilePhotometricInterpretation::YbrFull422
        }
        (color_space, channels, layout) => {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "uncompressed raw tiles hold interleaved grayscale, RGB or YCbCr samples, got \
                     {channels} {layout:?} {color_space:?} channels; read the stored byte \
                     ranges through tile_location"
                ),
            });
        }
    };
    let (bits, data) = match &tile.data {
        CpuTileData::U8(samples) => (8, samples.as_ref().clone()),
        CpuTileData::U16(samples) => (
            16,
            samples
                .iter()
                .flat_map(|sample| sample.to_ne_bytes())
                .collect(),
        ),
        CpuTileData::F32(_) => {
            return Err(WsiError::Unsupported {
                reason: "raw tiles cannot describe floating-point samples; read the stored byte \
                         ranges through tile_location"
                    .into(),
            });
        }
    };
    Ok(RawCompressedTile::builder(Compression::None)
        .dimensions(tile.width, tile.height)
        .bits_allocated(bits)
        .samples_per_pixel(tile.channels)
        .photometric_interpretation(photometric)
        .data(data)
        .build()?)
}
