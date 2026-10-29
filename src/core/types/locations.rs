//! Where tiles' encoded bytes are stored in a slide's files.
//!
//! A location answers "which bytes of which file hold this tile" without
//! reading or decoding the tile. Callers can copy the bytes unchanged, crop
//! JPEGs losslessly with external tools, or build byte-range references
//! (Zarr, kerchunk) that decode the original stream.

use std::path::Path;
use std::sync::Arc;

use super::{Compression, EncodedTilePhotometricInterpretation, LevelIdx};
use crate::error::WsiError;

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether this rectangle has area and lies inside `width` x `height`.
    pub fn fits_within(&self, width: u32, height: u32) -> bool {
        self.width > 0
            && self.height > 0
            && self
                .x
                .checked_add(self.width)
                .is_some_and(|right| right <= width)
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|bottom| bottom <= height)
    }
}

/// One contiguous byte range in a slide file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

impl ByteRange {
    pub const fn new(offset: u64, length: u64) -> Self {
        Self { offset, length }
    }

    /// The offset one past the last byte, or `None` when it overflows `u64`.
    pub fn end(&self) -> Option<u64> {
        self.offset.checked_add(self.length)
    }
}

/// How an encoded unit's segments relate to a standalone encoded stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StoredFraming {
    /// The segments, concatenated, are a complete stream that a standard
    /// decoder for the codec reads on its own: a JFIF, a JPEG 2000
    /// codestream, a JPEG XR image.
    Standalone,
    /// An abbreviated JPEG. The segments hold the tile's stream from its SOI
    /// marker, and the quantization and Huffman tables it references are
    /// [`EncodedCodec::jpeg_tables`] (TIFF `JPEGTables`). Decoders that take
    /// separate tables read it as is; [`EncodedUnit::assemble`] inserts the
    /// missing tables after SOI to make a standalone JPEG.
    JpegTables,
    /// A codec payload without container framing, such as a TIFF LZW,
    /// Deflate, Zstandard or uncompressed tile. Decode it with the
    /// parameters in [`EncodedUnit::codec`].
    BarePayload,
    /// A JPEG whose header wsi-rs derives from the file (NDPI levels). A
    /// standalone stream is [`EncodedUnit::prefix`], then the segments, then
    /// [`EncodedUnit::suffix`]. The prefix is the file's JPEG header with SOF
    /// dimensions set to this unit; when the segments are one restart
    /// interval, its restart interval is also cleared and the segments omit
    /// the trailing restart marker.
    Prefixed,
}

/// TIFF horizontal differencing applied before compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Predictor {
    #[default]
    None,
    /// TIFF predictor 2: integer horizontal differencing.
    Horizontal,
    /// TIFF predictor 3: floating-point horizontal differencing.
    FloatingPoint,
}

/// Whether samples of one pixel are stored together or plane by plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum PlanarConfiguration {
    /// Samples of each pixel are adjacent (TIFF `PlanarConfiguration` 1).
    #[default]
    Chunky,
    /// Each sample has its own plane (TIFF `PlanarConfiguration` 2).
    Planar,
}

/// Order of the color samples of one pixel in a bare payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ChannelOrder {
    /// Samples follow the photometric interpretation's order (R, G, B).
    #[default]
    Rgb,
    /// Color samples are stored B, G, R, as in CZI `Bgr24`/`Bgr48`.
    Bgr,
}

/// Byte order of multi-byte samples in an uncompressed or bare payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ByteOrder {
    LittleEndian,
    BigEndian,
}

/// What a decoder needs to read an encoded unit outside wsi-rs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EncodedCodec {
    compression: Compression,
    bits_per_sample: u16,
    significant_bits: Option<u16>,
    samples_per_pixel: u16,
    photometric_interpretation: EncodedTilePhotometricInterpretation,
    planar_configuration: PlanarConfiguration,
    channel_order: ChannelOrder,
    predictor: Predictor,
    byte_order: Option<ByteOrder>,
    chroma_subsampling: Option<(u8, u8)>,
    jpeg_tables: Option<Arc<[u8]>>,
}

impl EncodedCodec {
    /// A codec description with chunky samples, no predictor and no tables.
    pub fn new(
        compression: Compression,
        bits_per_sample: u16,
        samples_per_pixel: u16,
        photometric_interpretation: EncodedTilePhotometricInterpretation,
    ) -> Self {
        Self {
            compression,
            bits_per_sample,
            significant_bits: None,
            samples_per_pixel,
            photometric_interpretation,
            planar_configuration: PlanarConfiguration::Chunky,
            channel_order: ChannelOrder::Rgb,
            predictor: Predictor::None,
            byte_order: None,
            chroma_subsampling: None,
            jpeg_tables: None,
        }
    }

    #[must_use]
    pub fn with_planar_configuration(mut self, planar_configuration: PlanarConfiguration) -> Self {
        self.planar_configuration = planar_configuration;
        self
    }

    /// The low `bits` of each `bits_per_sample` container carry the sample,
    /// as for DICOM `BitsStored` 12 in 16-bit words; the high bits must be
    /// ignored.
    #[must_use]
    pub fn with_significant_bits(mut self, bits: u16) -> Self {
        self.significant_bits = Some(bits);
        self
    }

    #[must_use]
    pub fn with_channel_order(mut self, channel_order: ChannelOrder) -> Self {
        self.channel_order = channel_order;
        self
    }

    #[must_use]
    pub fn with_predictor(mut self, predictor: Predictor) -> Self {
        self.predictor = predictor;
        self
    }

    #[must_use]
    pub fn with_byte_order(mut self, byte_order: ByteOrder) -> Self {
        self.byte_order = Some(byte_order);
        self
    }

    /// Horizontal and vertical chroma subsampling factors, as container
    /// metadata states them (TIFF `YCbCrSubSampling`, DICOM photometric).
    #[must_use]
    pub fn with_chroma_subsampling(mut self, horizontal: u8, vertical: u8) -> Self {
        self.chroma_subsampling = Some((horizontal, vertical));
        self
    }

    /// Shared abbreviated-JPEG tables, as stored (SOI, DQT/DHT, EOI).
    #[must_use]
    pub fn with_jpeg_tables(mut self, jpeg_tables: Arc<[u8]>) -> Self {
        self.jpeg_tables = Some(jpeg_tables);
        self
    }

    pub fn compression(&self) -> Compression {
        self.compression
    }

    pub fn bits_per_sample(&self) -> u16 {
        self.bits_per_sample
    }

    /// Bits of each sample container that carry the sample, when fewer than
    /// [`Self::bits_per_sample`].
    pub fn significant_bits(&self) -> Option<u16> {
        self.significant_bits
    }

    pub fn samples_per_pixel(&self) -> u16 {
        self.samples_per_pixel
    }

    /// Color interpretation of the decoded samples. For JPEG, `Rgb` means
    /// the decoder must not apply a YCbCr transform even when the stream
    /// lacks an Adobe APP14 marker.
    pub fn photometric_interpretation(&self) -> EncodedTilePhotometricInterpretation {
        self.photometric_interpretation
    }

    pub fn planar_configuration(&self) -> PlanarConfiguration {
        self.planar_configuration
    }

    /// Order of a pixel's color samples in a bare payload.
    pub fn channel_order(&self) -> ChannelOrder {
        self.channel_order
    }

    pub fn predictor(&self) -> Predictor {
        self.predictor
    }

    /// Byte order of multi-byte samples, for payloads that store them raw.
    pub fn byte_order(&self) -> Option<ByteOrder> {
        self.byte_order
    }

    pub fn chroma_subsampling(&self) -> Option<(u8, u8)> {
        self.chroma_subsampling
    }

    pub fn jpeg_tables(&self) -> Option<&[u8]> {
        self.jpeg_tables.as_deref()
    }
}

/// One stored encoded image (a TIFF tile or strip, a JPEG file, a DICOM
/// frame, a CZI subblock) and the part of it that a tile uses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EncodedUnit {
    path: Arc<Path>,
    segments: Vec<ByteRange>,
    framing: StoredFraming,
    prefix: Arc<[u8]>,
    suffix: Arc<[u8]>,
    codec: EncodedCodec,
    stored_width: u32,
    stored_height: u32,
    source_region: PixelRect,
    tile_region: PixelRect,
}

impl EncodedUnit {
    /// Starts a unit stored in `path` with the given encoded image size.
    /// The source and tile regions default to the whole stored image.
    pub fn builder(
        path: Arc<Path>,
        codec: EncodedCodec,
        stored_width: u32,
        stored_height: u32,
    ) -> EncodedUnitBuilder {
        let whole = PixelRect::new(0, 0, stored_width, stored_height);
        EncodedUnitBuilder {
            unit: Self {
                path,
                segments: Vec::new(),
                framing: StoredFraming::Standalone,
                prefix: Arc::from(&[][..]),
                suffix: Arc::from(&[][..]),
                codec,
                stored_width,
                stored_height,
                source_region: whole,
                tile_region: whole,
            },
        }
    }

    /// The slide file that holds the segments.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Byte ranges of the stored stream, in stream order.
    pub fn segments(&self) -> &[ByteRange] {
        &self.segments
    }

    pub fn framing(&self) -> StoredFraming {
        self.framing
    }

    /// Bytes placed before the segments to form a standalone stream; empty
    /// unless the framing is [`StoredFraming::Prefixed`].
    pub fn prefix(&self) -> &[u8] {
        &self.prefix
    }

    /// Bytes placed after the segments to form a standalone stream; empty
    /// unless the framing is [`StoredFraming::Prefixed`].
    pub fn suffix(&self) -> &[u8] {
        &self.suffix
    }

    pub fn codec(&self) -> &EncodedCodec {
        &self.codec
    }

    /// Size of the stored encoded image, including any padding a TIFF tile
    /// carries past the image edge.
    pub fn stored_size(&self) -> (u32, u32) {
        (self.stored_width, self.stored_height)
    }

    /// The part of the stored image the tile uses. Equal to the whole stored
    /// image unless the tile is cut from a larger unit or clipped at an edge.
    pub fn source_region(&self) -> PixelRect {
        self.source_region
    }

    /// Where [`Self::source_region`] lands in the requested tile.
    pub fn tile_region(&self) -> PixelRect {
        self.tile_region
    }

    /// Total stored bytes across all segments, or `None` on overflow.
    pub fn stored_len(&self) -> Option<u64> {
        self.segments
            .iter()
            .try_fold(0u64, |total, segment| total.checked_add(segment.length))
    }

    /// Builds the standalone stream from the bytes of [`Self::segments`],
    /// read by the caller in the same order.
    ///
    /// For a unit that covers its whole tile, the result equals
    /// [`crate::Slide::read_raw_compressed_tile`]'s data, with one exception:
    /// a bare payload whose decoding needs parameters a
    /// [`crate::RawCompressedTile`] cannot carry (a predictor, separate
    /// sample planes, a non-native byte order, B, G, R sample order, masked
    /// high bits). Raw reads return such tiles as exact uncompressed samples
    /// instead; subsampled YCbCr payloads have no raw read. Bare payloads are
    /// returned concatenated; they need [`Self::codec`] to decode.
    pub fn assemble<'a>(
        &self,
        segment_bytes: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Vec<u8>, WsiError> {
        let expected = self.stored_len().ok_or_else(|| WsiError::Unsupported {
            reason: "encoded unit segment lengths overflow u64".into(),
        })?;
        let (prefix, suffix) = match self.framing {
            StoredFraming::Prefixed => (&self.prefix[..], &self.suffix[..]),
            _ => (&[][..], &[][..]),
        };
        // One buffer for prefix, segments and suffix, so a framed stream holds
        // the payload once beside the caller's bytes.
        let capacity = usize::try_from(expected)
            .ok()
            .and_then(|len| len.checked_add(prefix.len() + suffix.len()))
            .ok_or_else(|| WsiError::Unsupported {
                reason: "encoded unit is too large to assemble in memory".into(),
            })?;
        let mut stream = Vec::new();
        stream
            .try_reserve_exact(capacity)
            .map_err(|err| WsiError::Unsupported {
                reason: format!("encoded unit assembly allocation failed: {err}"),
            })?;
        stream.extend_from_slice(prefix);
        let mut segment_count = 0usize;
        for (index, bytes) in segment_bytes.into_iter().enumerate() {
            let range = self
                .segments
                .get(index)
                .ok_or_else(|| WsiError::Unsupported {
                    reason: format!(
                        "encoded unit has {} segments, but more were supplied",
                        self.segments.len()
                    ),
                })?;
            if bytes.len() as u64 != range.length {
                return Err(WsiError::Unsupported {
                    reason: format!(
                        "encoded unit segment {index} is {} bytes, expected {}",
                        bytes.len(),
                        range.length
                    ),
                });
            }
            stream.extend_from_slice(bytes);
            segment_count += 1;
        }
        if segment_count != self.segments.len() {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "encoded unit has {} segments, but {segment_count} were supplied",
                    self.segments.len()
                ),
            });
        }
        stream.extend_from_slice(suffix);
        match self.framing {
            StoredFraming::JpegTables => crate::decode::jpeg::frame::merge_missing_jpeg_tables(
                stream,
                self.codec.jpeg_tables(),
            ),
            _ => Ok(stream),
        }
    }
}

/// Builder for [`EncodedUnit`].
#[derive(Debug, Clone)]
#[must_use]
pub struct EncodedUnitBuilder {
    unit: EncodedUnit,
}

impl EncodedUnitBuilder {
    /// Appends a byte range of the stored stream.
    pub fn segment(mut self, range: ByteRange) -> Self {
        self.unit.segments.push(range);
        self
    }

    pub fn segments(mut self, ranges: Vec<ByteRange>) -> Self {
        self.unit.segments = ranges;
        self
    }

    pub fn framing(mut self, framing: StoredFraming) -> Self {
        self.unit.framing = framing;
        self
    }

    /// Header and trailer bytes for [`StoredFraming::Prefixed`] units.
    pub fn prefix_suffix(mut self, prefix: Arc<[u8]>, suffix: Arc<[u8]>) -> Self {
        self.unit.prefix = prefix;
        self.unit.suffix = suffix;
        self
    }

    pub fn source_region(mut self, region: PixelRect) -> Self {
        self.unit.source_region = region;
        self
    }

    pub fn tile_region(mut self, region: PixelRect) -> Self {
        self.unit.tile_region = region;
        self
    }

    /// Validates the unit against `file_len`, the size of [`EncodedUnit::path`].
    pub fn build(self, file_len: u64) -> Result<EncodedUnit, WsiError> {
        let unit = self.unit;
        let invalid = |reason: String| WsiError::Unsupported {
            reason: format!("invalid encoded unit location: {reason}"),
        };
        if unit.segments.is_empty() {
            return Err(invalid("no byte segments".into()));
        }
        for (index, segment) in unit.segments.iter().enumerate() {
            if segment.length == 0 {
                return Err(invalid(format!("segment {index} is empty")));
            }
            match segment.end() {
                Some(end) if end <= file_len => {}
                _ => {
                    return Err(invalid(format!(
                        "segment {index} ({}+{}) exceeds the {file_len}-byte file",
                        segment.offset, segment.length
                    )))
                }
            }
        }
        if unit.stored_len().is_none() {
            return Err(invalid("segment lengths overflow u64".into()));
        }
        if unit.framing != StoredFraming::Prefixed
            && (!unit.prefix.is_empty() || !unit.suffix.is_empty())
        {
            return Err(invalid(
                "prefix or suffix bytes need Prefixed framing".into(),
            ));
        }
        if unit.framing == StoredFraming::JpegTables && unit.codec.jpeg_tables.is_none() {
            return Err(invalid("JpegTables framing needs codec JPEG tables".into()));
        }
        if !unit
            .source_region
            .fits_within(unit.stored_width, unit.stored_height)
        {
            return Err(invalid(format!(
                "source region {:?} is outside the {}x{} stored image",
                unit.source_region, unit.stored_width, unit.stored_height
            )));
        }
        if unit.tile_region.width == 0 || unit.tile_region.height == 0 {
            return Err(invalid("tile region is empty".into()));
        }
        Ok(unit)
    }
}

/// Stored encoded bytes that make up one tile.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoredTile {
    tile_width: u32,
    tile_height: u32,
    units: Vec<EncodedUnit>,
}

impl StoredTile {
    /// A tile of `tile_width` x `tile_height` pixels built from `units`.
    /// Each unit's tile region must lie inside the tile.
    pub fn new(
        tile_width: u32,
        tile_height: u32,
        units: Vec<EncodedUnit>,
    ) -> Result<Self, WsiError> {
        if units.is_empty() {
            return Err(WsiError::Unsupported {
                reason: "stored tile location needs at least one encoded unit".into(),
            });
        }
        if let Some(unit) = units
            .iter()
            .find(|unit| !unit.tile_region.fits_within(tile_width, tile_height))
        {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "encoded unit tile region {:?} is outside the {tile_width}x{tile_height} tile",
                    unit.tile_region
                ),
            });
        }
        Ok(Self {
            tile_width,
            tile_height,
            units,
        })
    }

    /// Logical tile size: the requested tile, clipped at the level edge.
    pub fn tile_size(&self) -> (u32, u32) {
        (self.tile_width, self.tile_height)
    }

    /// One unit for most formats; several when the tile is assembled from
    /// stored units, such as strips of a strip-based TIFF.
    pub fn units(&self) -> &[EncodedUnit] {
        &self.units
    }

    /// The single unit, when one stored image covers the whole tile.
    pub fn single_unit(&self) -> Option<&EncodedUnit> {
        match self.units.as_slice() {
            [unit] => Some(unit),
            _ => None,
        }
    }
}

/// Why no byte range holds a tile's encoded pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NotAddressableReason {
    /// wsi-rs computes the level by downsampling `base_level` by `factor`.
    SyntheticLevel { base_level: LevelIdx, factor: u32 },
    /// The tile is resampled from stored pixels at a non-integer scale.
    Resampled,
    /// The tile is a window decoded from one codestream that has no
    /// per-tile byte ranges (a raw JPEG 2000 file read as region tiles).
    CodestreamWindow,
    /// The tile's pixels are not stored in the slide's files, for example an
    /// image that wsi-rs only reads after converting it.
    NotStored,
}

/// Where a tile's encoded bytes are, from [`crate::Slide::tile_location`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TileLocation {
    /// Stored encoded bytes make up this tile.
    Stored(StoredTile),
    /// The file stores no pixels for this tile (a sparse grid entry, an empty
    /// TIFF tile, a VSI background tile). Readers paint background.
    Absent,
    /// No byte range holds this tile's encoded pixels.
    NotAddressable(NotAddressableReason),
}

/// One entry of [`LevelTileLocations`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LevelTileLocation {
    pub col: i64,
    pub row: i64,
    pub location: TileLocation,
}

impl LevelTileLocation {
    pub fn new(col: i64, row: i64, location: TileLocation) -> Self {
        Self { col, row, location }
    }
}

/// Every tile location of one level plane, from
/// [`crate::Slide::level_tile_locations`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LevelTileLocations {
    /// Nominal tile size of the level grid.
    pub tile_width: u32,
    pub tile_height: u32,
    /// Grid shape; for irregular layouts, the extent of the stored entries.
    pub tiles_across: u64,
    pub tiles_down: u64,
    /// Tiles in row-major order.
    pub tiles: Vec<LevelTileLocation>,
}
