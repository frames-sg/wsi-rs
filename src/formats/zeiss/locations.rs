//! Where CZI tiles are stored, and raw tiles built from the subblocks that
//! paint them.
//!
//! A tile of the regular output grid is painted from the subblocks the
//! directory places over it, in the order tile composition uses: by `M`
//! index, then file position, later subblocks over earlier ones. Each one
//! contributes the part of its stored image that falls inside the tile.

use super::slide::ZeissSlide;
use super::*;
use crate::decode::jpeg::lossless::{
    lossless_crop_coefficient_budget, lossless_crop_or_whole_jpeg,
};

/// One subblock's contribution to a tile.
struct SubblockPlacement {
    info: DirectorySubBlockInfo,
    /// The part of the stored image inside the tile.
    source: PixelRect,
    /// Where `source` lands in the tile.
    target: PixelRect,
}

/// The subblocks that paint one tile, in paint order.
struct TilePlacements {
    width: u32,
    height: u32,
    placements: Vec<SubblockPlacement>,
}

impl TilePlacements {
    fn covers_whole_tile(&self, placement: &SubblockPlacement) -> bool {
        placement.target == PixelRect::new(0, 0, self.width, self.height)
    }
}

impl ZeissSlide {
    /// Locations of every subblock that paints `req`, in paint order. Units
    /// overlap where subblocks overlap; later units are drawn over earlier
    /// ones, and units hidden entirely by a later one are left out.
    pub(super) fn tile_location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        let tile = self.tile_placements(req)?;
        if tile.placements.is_empty() {
            return Ok(TileLocation::Absent);
        }
        let units = tile
            .placements
            .iter()
            .map(|placement| self.encoded_unit(placement))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TileLocation::Stored(StoredTile::new(
            tile.width,
            tile.height,
            units,
        )?))
    }

    /// A raw tile from the subblocks that paint `req`:
    ///
    /// - one classic-JPEG subblock that is exactly the tile: its stream;
    /// - one classic-JPEG subblock that covers the tile: a lossless
    ///   coefficient-domain crop starting at the MCU corner at or before the
    ///   tile, with [`RawCompressedTile::tile_region`] locating the tile, or
    ///   the whole subblock stream with the tile located in it when the crop
    ///   does not fit `crop_working_set` bytes;
    /// - one JPEG XR subblock that covers the tile: its whole stream, with
    ///   [`RawCompressedTile::tile_region`] locating the tile in it;
    /// - only uncompressed subblocks: the exact tile as uncompressed
    ///   interleaved RGB samples, lossless because no lossy codec is
    ///   involved (CZI stores B, G, R; the samples are reordered to RGB).
    ///
    /// Other lossy compositions have no single stored stream and fail with
    /// [`WsiError::Unsupported`]; [`Self::tile_location`] reports their parts.
    pub(super) fn read_raw_tile(
        &self,
        req: &TileRequest,
        crop_working_set: u64,
    ) -> Result<RawCompressedTile, WsiError> {
        let tile = self.tile_placements(req)?;
        let unsupported = |detail: String| WsiError::Unsupported {
            reason: format!(
                "Zeiss tile ({}, {}) at level {} {detail}",
                req.col,
                req.row,
                req.level.get()
            ),
        };
        let lossless = |placement: &SubblockPlacement| {
            placement.info.compression == CziCompressionMode::UnCompressed
        };
        match tile.placements.as_slice() {
            [] => Err(unsupported(
                "has no stored subblock; tile_location reports it Absent".into(),
            )),
            placements if placements.iter().all(lossless) => self.raw_uncompressed_samples(req),
            [placement] if tile.covers_whole_tile(placement) => {
                self.raw_single_subblock(placement, crop_working_set, &unsupported)
            }
            placements => Err(unsupported(format!(
                "is composed from {} subblocks or covered only in part, and its lossy subblocks have no single stored stream; tile_location reports each subblock and its placement",
                placements.len()
            ))),
        }
    }

    /// [`SlideReader::raw_compressed_tile_reservation_hint`] for `req`: the
    /// lossless-crop limit when the tile is cropped from one JPEG subblock,
    /// otherwise 0, leaving one encoded unit. It follows
    /// [`Self::read_raw_tile`]'s cases.
    pub(super) fn raw_tile_reservation_hint(&self, req: &TileRequest) -> Result<u64, WsiError> {
        let tile = self.tile_placements(req)?;
        let crops_jpeg = match tile.placements.as_slice() {
            [placement] if tile.covers_whole_tile(placement) => {
                let info = &placement.info;
                info.compression == CziCompressionMode::Jpg
                    && placement.source
                        != PixelRect::new(0, 0, info.stored_size.w, info.stored_size.h)
            }
            _ => false,
        };
        Ok(if crops_jpeg {
            self.limits.lossless_crop_bytes()
        } else {
            0
        })
    }

    /// Raw tile for a tile that lies entirely inside one lossy subblock.
    fn raw_single_subblock(
        &self,
        placement: &SubblockPlacement,
        crop_working_set: u64,
        unsupported: &dyn Fn(String) -> WsiError,
    ) -> Result<RawCompressedTile, WsiError> {
        let info = &placement.info;
        let stored = (info.stored_size.w, info.stored_size.h);
        let whole = placement.source == PixelRect::new(0, 0, stored.0, stored.1);
        match info.compression {
            CziCompressionMode::Jpg if whole => self.raw_jpeg_subblock(info),
            CziCompressionMode::Jpg => self.raw_jpeg_crop(placement, crop_working_set),
            CziCompressionMode::JpgXr => {
                let (codec, _) = subblock_codec(info)?;
                let raw = self.read_source_subblock(info)?;
                let mut builder = RawCompressedTile::builder(Compression::JpegXr)
                    .dimensions(stored.0, stored.1)
                    .bits_allocated(codec.bits_per_sample())
                    .samples_per_pixel(codec.samples_per_pixel())
                    .photometric_interpretation(codec.photometric_interpretation())
                    .data(raw.data);
                if !whole {
                    builder = builder.tile_region(placement.source);
                }
                Ok(builder.build()?)
            }
            other => Err(unsupported(format!(
                "lies in a {other} subblock, which has no raw stream"
            ))),
        }
    }

    /// The tile cut from one JPEG subblock in the coefficient domain:
    /// quantized coefficients are copied, never decoded to pixels. The output
    /// starts at the MCU corner at or before the tile.
    ///
    /// The subblock bytes, the coefficient planes and the worst-case output
    /// share `crop_working_set`, which admission reserved for the read. A
    /// crop needing more coefficients than remain returns the whole subblock
    /// stream instead, with [`RawCompressedTile::tile_region`] locating the
    /// tile; this is decided before entropy decoding.
    fn raw_jpeg_crop(
        &self,
        placement: &SubblockPlacement,
        crop_working_set: u64,
    ) -> Result<RawCompressedTile, WsiError> {
        let data = self.read_source_subblock(&placement.info)?.data;
        let source = placement.source;
        let crop = (source.width, source.height);
        let budget = lossless_crop_coefficient_budget(crop_working_set, data.len() as u64, crop);
        let crop = lossless_crop_or_whole_jpeg(
            data,
            (source.x, source.y, source.width, source.height),
            budget,
            self.limits.encoded_unit_bytes(),
        )?;
        self.within_encoded_unit(crop.into_raw_tile((source.width, source.height))?)
    }

    /// Refuses a built raw tile larger than one encoded unit, the admission
    /// bound this reader promises for every tile. Crops and composed samples
    /// are built rather than stored, so the subblock preflight does not
    /// bound them.
    fn within_encoded_unit(&self, tile: RawCompressedTile) -> Result<RawCompressedTile, WsiError> {
        let limit = self.limits.encoded_unit_bytes();
        let requested = tile.data().len() as u64;
        if requested > limit {
            return Err(WsiError::ResourceLimit {
                resource: "encoded tile/frame unit",
                requested,
                limit,
            });
        }
        Ok(tile)
    }

    fn raw_jpeg_subblock(
        &self,
        info: &DirectorySubBlockInfo,
    ) -> Result<RawCompressedTile, WsiError> {
        let raw = self.read_source_subblock(info)?;
        let tile = crate::decode::jpeg::standalone_raw_jpeg_tile(raw.data)?;
        if (tile.width(), tile.height()) != (info.stored_size.w, info.stored_size.h) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "Zeiss raw JPEG dimensions {}x{} do not match stored subblock {}x{}",
                    tile.width(),
                    tile.height(),
                    info.stored_size.w,
                    info.stored_size.h
                ),
            });
        }
        Ok(tile)
    }

    /// The composed tile's exact RGB samples, for tiles whose subblocks
    /// are all uncompressed.
    fn raw_uncompressed_samples(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        let tile = self.read_tile(
            req.scene.get(),
            req.series.get(),
            req.level.get(),
            req.col,
            req.row,
        )?;
        let (width, height) = (tile.width, tile.height);
        let (3, CpuTileLayout::Interleaved, CpuTileData::U8(samples)) =
            (tile.channels, tile.layout, tile.data)
        else {
            return Err(WsiError::Unsupported {
                reason: "Zeiss uncompressed tiles must compose to interleaved 8-bit RGB".into(),
            });
        };
        self.within_encoded_unit(
            RawCompressedTile::builder(Compression::None)
                .dimensions(width, height)
                .bits_allocated(8)
                .samples_per_pixel(3)
                .photometric_interpretation(EncodedTilePhotometricInterpretation::Rgb)
                .data(Arc::unwrap_or_clone(samples))
                .build()?,
        )
    }

    fn tile_placements(&self, req: &TileRequest) -> Result<TilePlacements, WsiError> {
        let (_, series, level) = self.dataset.level(req.scene, req.series, req.level)?;
        series.axes.validate_plane(req.plane.get())?;
        let Some(tile) = level.regular_tile(req)? else {
            return Err(WsiError::UnsupportedFormat(
                "Zeiss levels must use regular tiles".into(),
            ));
        };
        let ratio = level.downsample.round().max(1.0) as i64;
        let candidates = self
            .canvas_level_tile_subblocks
            .get(req.level.get() as usize)
            .and_then(|tiles| tiles.get(&(req.col, req.row)))
            .map_or(&[][..], Vec::as_slice);
        let czi = self.czi.lock().unwrap_or_else(|error| error.into_inner());
        let mut placements = Vec::with_capacity(candidates.len());
        for &index in candidates {
            let info = czi.subblocks().get(index).ok_or_else(|| {
                WsiError::DisplayConversion(format!("Zeiss subblock index {index} out of range"))
            })?;
            if let Some((source, target)) =
                placement_in_tile(info, self.subblock_origin, ratio, &tile)
            {
                placements.push(SubblockPlacement {
                    info: info.clone(),
                    source,
                    target,
                });
            }
        }
        drop(czi);
        placements.sort_by_key(|placement| {
            (
                placement.info.m_index.unwrap_or(i32::MIN),
                placement.info.file_position,
            )
        });
        let mut tile = TilePlacements {
            width: tile.width,
            height: tile.height,
            placements,
        };
        // Blits are opaque, so a unit covering the whole tile hides every
        // unit drawn before it.
        if let Some(last_cover) = tile
            .placements
            .iter()
            .rposition(|placement| tile.covers_whole_tile(placement))
        {
            tile.placements.drain(..last_cover);
        }
        Ok(tile)
    }

    fn encoded_unit(&self, placement: &SubblockPlacement) -> Result<EncodedUnit, WsiError> {
        let info = &placement.info;
        let (codec, framing) = subblock_codec(info)?;
        let data = self.subblock_data.get(info.index).ok_or_else(|| {
            WsiError::DisplayConversion(format!(
                "Zeiss subblock index {} has no preflighted data range",
                info.index
            ))
        })?;
        EncodedUnit::builder(
            Arc::clone(&self.location_path),
            codec,
            info.stored_size.w,
            info.stored_size.h,
        )
        .segment(*data)
        .framing(framing)
        .source_region(placement.source)
        .tile_region(placement.target)
        .build(self.source_identity.length())
    }
}

/// Where subblock `info` falls in `tile`, placed as tile composition places
/// it: its scaled directory origin, at its full stored size. Returns the
/// stored-image source rectangle and the tile target rectangle, or `None`
/// when they do not intersect.
fn placement_in_tile(
    info: &DirectorySubBlockInfo,
    origin: (i32, i32),
    ratio: i64,
    tile: &RegularTile,
) -> Option<(PixelRect, PixelRect)> {
    let left = (i64::from(info.rect.x) - i64::from(origin.0)).div_euclid(ratio)
        - i64::try_from(tile.x).ok()?;
    let top = (i64::from(info.rect.y) - i64::from(origin.1)).div_euclid(ratio)
        - i64::try_from(tile.y).ok()?;
    let x0 = left.max(0);
    let y0 = top.max(0);
    let x1 = (left + i64::from(info.stored_size.w)).min(i64::from(tile.width));
    let y1 = (top + i64::from(info.stored_size.h)).min(i64::from(tile.height));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    // Every value lies inside a u32 stored image or tile.
    let rect =
        |x: i64, y: i64| PixelRect::new(x as u32, y as u32, (x1 - x0) as u32, (y1 - y0) as u32);
    Some((rect(x0 - left, y0 - top), rect(x0, y0)))
}

/// Decoder parameters and framing of one subblock's data part.
///
/// Uncompressed and Zstandard data store samples interleaved in the CZI
/// pixel type's B, G, R order (little-endian for 16-bit samples), which the
/// codec states as [`ChannelOrder::Bgr`]. JPEG and JPEG XR streams decode to
/// RGB with standard decoders.
fn subblock_codec(info: &DirectorySubBlockInfo) -> Result<(EncodedCodec, StoredFraming), WsiError> {
    use EncodedTilePhotometricInterpretation::{Monochrome2, Rgb, YbrFull422};
    let (bits, samples, photometric) = match info.pixel_type {
        CziPixelType::Gray8 => (8, 1, Monochrome2),
        CziPixelType::Gray16 => (16, 1, Monochrome2),
        CziPixelType::Bgr24 => (8, 3, Rgb),
        CziPixelType::Bgr48 => (16, 3, Rgb),
        other => {
            return Err(WsiError::Unsupported {
                reason: format!("CZI {other} subblocks have no encoded-unit description"),
            })
        }
    };
    let bare = |compression| {
        let codec = EncodedCodec::new(compression, bits, samples, photometric);
        let codec = if samples == 3 {
            codec.with_channel_order(ChannelOrder::Bgr)
        } else {
            codec
        };
        let codec = if bits > 8 {
            codec.with_byte_order(ByteOrder::LittleEndian)
        } else {
            codec
        };
        (codec, StoredFraming::BarePayload)
    };
    Ok(match info.compression {
        CziCompressionMode::UnCompressed => bare(Compression::None),
        CziCompressionMode::Zstd0 => bare(Compression::Zstd),
        CziCompressionMode::Jpg => {
            // JFIF color JPEGs carry YCbCr; a standard decoder converts them.
            let photometric = if samples == 3 {
                YbrFull422
            } else {
                Monochrome2
            };
            (
                EncodedCodec::new(Compression::Jpeg, bits, samples, photometric),
                StoredFraming::Standalone,
            )
        }
        CziCompressionMode::JpgXr => (
            EncodedCodec::new(Compression::JpegXr, bits, samples, photometric),
            StoredFraming::Standalone,
        ),
        CziCompressionMode::Zstd1 => return Err(WsiError::Unsupported {
            reason:
                "CZI Zstd1 subblocks start with a packing header that EncodedCodec cannot describe"
                    .into(),
        }),
    })
}
