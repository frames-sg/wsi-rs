//! Where MIRAX tiles are stored, and raw tiles cut from the stored records.
//!
//! Each index record is one complete image file (JPEG, PNG or BMP) inside a
//! data file. A logical tile is either that whole image, an integer-offset
//! crop of it, or a fractional subtile that wsi-rs resamples.

use super::*;
use crate::decode::jpeg::lossless::{
    lossless_crop_coefficient_bound, lossless_crop_coefficient_budget, lossless_crop_or_whole_jpeg,
    lossless_crop_output_bound,
};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const PNG_IHDR_END: usize = 8 + 8 + 13;
const PNG_COLOR_RGB: u8 = 2;
const BMP_INFO_HEADER_END: usize = 14 + 40;

impl MiraxImageFormat {
    pub(super) fn compression(self) -> Compression {
        match self {
            Self::Jpeg => Compression::Jpeg,
            Self::Png => Compression::Png,
            Self::Bmp24 => Compression::Bmp,
        }
    }

    /// Decoder parameters of one stored record, as the slide declares them:
    /// MIRAX stores 8-bit color images, and its JPEGs follow the JFIF
    /// convention, so a standard decoder applies the YCbCr transform.
    fn encoded_codec(self) -> EncodedCodec {
        match self {
            Self::Jpeg => EncodedCodec::new(
                Compression::Jpeg,
                8,
                3,
                EncodedTilePhotometricInterpretation::YbrFull422,
            ),
            Self::Png | Self::Bmp24 => EncodedCodec::new(
                self.compression(),
                8,
                3,
                EncodedTilePhotometricInterpretation::Rgb,
            ),
        }
    }
}

impl MiraxSlide {
    /// The stored record that holds `tile`, with the `width` x `height`
    /// logical tile located inside it.
    fn encoded_unit(
        &self,
        tile: &MiraxTile,
        width: u32,
        height: u32,
    ) -> Result<EncodedUnit, WsiError> {
        let image = &tile.image;
        let record = &image.record;
        let file_len = self
            .shared
            .datafile_identities
            .get(&*record.path)
            .ok_or_else(|| {
                WsiError::invalid_slide(&record.path, "MIRAX record names an unlisted data file")
            })?
            .length();
        // Non-resampled descriptors hold whole-pixel source offsets.
        let source = PixelRect::new(tile.src_x as u32, tile.src_y as u32, width, height);
        EncodedUnit::builder(
            Arc::clone(&record.path),
            image.format.encoded_codec(),
            image.expected_width,
            image.expected_height,
        )
        .segment(ByteRange::new(record.offset, record.len))
        .framing(StoredFraming::Standalone)
        .source_region(source)
        .tile_region(PixelRect::new(0, 0, width, height))
        .build(file_len)
    }
}

impl MiraxReader {
    pub(super) fn location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        let Some((entry, tile)) = self.stored_tile_for_request(req)? else {
            return Ok(TileLocation::Absent);
        };
        if tile.image.resample {
            return Ok(TileLocation::NotAddressable(
                NotAddressableReason::Resampled,
            ));
        }
        let (width, height) = entry.dimensions;
        let unit = self.slide.encoded_unit(tile, width, height)?;
        Ok(TileLocation::Stored(StoredTile::new(
            width,
            height,
            vec![unit],
        )?))
    }

    pub(super) fn raw_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        let (entry, tile) = self.tile_for_request(req)?;
        let image = &tile.image;
        if image.resample {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "MIRAX tile ({}, {}) at level {} is resampled from a fractional subtile, so no stored bytes hold it; tile_location reports NotAddressable(Resampled)",
                    req.col,
                    req.row,
                    req.level.get()
                ),
            });
        }
        let whole_image = tile.src_x == 0.0
            && tile.src_y == 0.0
            && entry.dimensions == (image.expected_width, image.expected_height);
        match (image.format, whole_image) {
            (MiraxImageFormat::Jpeg, true) => self.raw_whole_jpeg(image, entry.dimensions),
            (MiraxImageFormat::Jpeg, false) => {
                let budget = self.jpeg_crop_budget(entry, tile).ok_or_else(|| {
                    WsiError::tile_read(req, "MIRAX JPEG crop tile has no crop budget")
                })?;
                self.raw_jpeg_crop(tile, entry.dimensions, budget)
            }
            (_, true) => self.raw_whole_raster(image),
            (_, false) => self.raw_raster_crop(req),
        }
    }

    fn raw_whole_jpeg(
        &self,
        image: &MiraxImage,
        dimensions: (u32, u32),
    ) -> Result<RawCompressedTile, WsiError> {
        let data = self.slide.read_record_bytes(&image.record)?;
        let raw = crate::decode::jpeg::standalone_raw_jpeg_tile(data)?;
        if (raw.width(), raw.height()) != dimensions {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "MIRAX raw JPEG geometry {}x{} does not exactly match logical tile {}x{}",
                    raw.width(),
                    raw.height(),
                    dimensions.0,
                    dimensions.1
                ),
            });
        }
        Ok(raw)
    }

    /// A tile cut from a stored JPEG at a whole-pixel offset, cropped in the
    /// coefficient domain: quantized coefficients are copied, never decoded
    /// to pixels. The output starts at the MCU corner at or before the tile.
    ///
    /// The record, the coefficient planes and the worst-case output share
    /// `working_set`, which admission reserved for this tile. A crop needing
    /// more coefficients than remain, such as one of a JPEG larger than the
    /// slide declares, returns the whole record instead, with `tile_region`
    /// locating the tile; this is decided before entropy decoding.
    fn raw_jpeg_crop(
        &self,
        tile: &MiraxTile,
        (width, height): (u32, u32),
        working_set: u64,
    ) -> Result<RawCompressedTile, WsiError> {
        let data = self.slide.read_record_bytes(&tile.image.record)?;
        let budget =
            lossless_crop_coefficient_budget(working_set, data.len() as u64, (width, height));
        // Non-resampled descriptors hold whole-pixel source offsets.
        let rect = (tile.src_x as u32, tile.src_y as u32, width, height);
        lossless_crop_or_whole_jpeg(data, rect, budget, self.slide.encoded_unit_bytes)?
            .into_raw_tile((width, height))
    }

    /// A PNG or BMP record returned as stored, once its header confirms the
    /// 8-bit RGB image that [`Self::location`] reports.
    fn raw_whole_raster(&self, image: &MiraxImage) -> Result<RawCompressedTile, WsiError> {
        let data = self.slide.read_record_bytes(&image.record)?;
        let dimensions = match image.format {
            MiraxImageFormat::Png => png_rgb8_dimensions(&data),
            MiraxImageFormat::Bmp24 => bmp24_dimensions(&data),
            MiraxImageFormat::Jpeg => unreachable!("JPEG records use raw_whole_jpeg"),
        }
        .ok_or_else(|| WsiError::Unsupported {
            reason: format!(
                "MIRAX {:?} record at offset {} is not an 8-bit RGB image of the declared format",
                image.format, image.record.offset
            ),
        })?;
        if dimensions != (image.expected_width, image.expected_height) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "MIRAX {:?} record is {}x{}, but the slide declares {}x{} images",
                    image.format,
                    dimensions.0,
                    dimensions.1,
                    image.expected_width,
                    image.expected_height
                ),
            });
        }
        Ok(RawCompressedTile::builder(image.format.compression())
            .dimensions(dimensions.0, dimensions.1)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::Rgb)
            .data(data)
            .build()?)
    }

    /// A tile cropped from a PNG or BMP image, as exact interleaved RGB
    /// samples. Both codecs are lossless, so no information is lost.
    fn raw_raster_crop(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        let tile = self.decode_tile(req)?;
        let (width, height) = (tile.width, tile.height);
        let (3, CpuTileLayout::Interleaved, CpuTileData::U8(samples)) =
            (tile.channels, tile.layout, tile.data)
        else {
            return Err(WsiError::Unsupported {
                reason: "MIRAX PNG/BMP tiles must decode to interleaved 8-bit RGB".into(),
            });
        };
        Ok(RawCompressedTile::builder(Compression::None)
            .dimensions(width, height)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::Rgb)
            .data(Arc::unwrap_or_clone(samples))
            .build()?)
    }
}

/// Bytes a raw lossless crop of a JPEG crop tile needs, or `None` for tiles
/// that are not cut from a stored JPEG at a whole-pixel offset: the record,
/// the coefficient planes of the JPEG at its declared size, and the
/// worst-case output.
pub(super) fn jpeg_crop_reservation(entry: &TileEntry, tile: &MiraxTile) -> Option<u64> {
    let image = &tile.image;
    let whole_image = tile.src_x == 0.0
        && tile.src_y == 0.0
        && entry.dimensions == (image.expected_width, image.expected_height);
    if image.format != MiraxImageFormat::Jpeg || image.resample || whole_image {
        return None;
    }
    let source_len = image.record.len;
    let coefficients = lossless_crop_coefficient_bound(
        (image.expected_width, image.expected_height),
        entry.dimensions,
    );
    let output = lossless_crop_output_bound(source_len, entry.dimensions.0, entry.dimensions.1);
    // The record, the marker segments parsing copies out of it, the
    // coefficients and the output; `lossless_crop_coefficient_budget` sets
    // aside the same amounts.
    Some(
        source_len
            .saturating_mul(2)
            .saturating_add(coefficients)
            .saturating_add(output),
    )
}

/// Dimensions of an 8-bit truecolor PNG, from its leading IHDR chunk.
fn png_rgb8_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let header = data.get(..PNG_IHDR_END)?;
    let be32 = |offset: usize| u32::from_be_bytes(header[offset..offset + 4].try_into().unwrap());
    let rgb8 = header.starts_with(PNG_SIGNATURE)
        && be32(8) == 13
        && &header[12..16] == b"IHDR"
        && header[24] == 8
        && header[25] == PNG_COLOR_RGB;
    rgb8.then(|| (be32(16), be32(20)))
}

/// Dimensions of an uncompressed 24-bit BMP with a BITMAPINFOHEADER or a
/// later header version.
fn bmp24_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let header = data.get(..BMP_INFO_HEADER_END)?;
    let le16 = |offset: usize| u16::from_le_bytes([header[offset], header[offset + 1]]);
    let le32 = |offset: usize| u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
    let rgb24 = header.starts_with(b"BM")
        && le32(14) >= 40
        && le16(26) == 1
        && le16(28) == 24
        && le32(30) == 0;
    let width = i32::from_le_bytes(le32(18).to_le_bytes());
    let height = i32::from_le_bytes(le32(22).to_le_bytes());
    // Negative heights mark top-down row order.
    (rgb24 && width > 0 && height != 0).then(|| (width.unsigned_abs(), height.unsigned_abs()))
}
