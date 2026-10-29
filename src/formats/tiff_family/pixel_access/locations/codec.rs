//! Codec descriptions of stored TIFF tiles and strips, from IFD tags.

use super::*;
use crate::decode::jpeg::frame::jpeg_sof_color_hint;
use crate::decode::jpeg::is_sof_marker;

/// Bytes of a stored JPEG unit one positioned read takes while its marker
/// segments are walked to the color markers; a longer segment is skipped by
/// its stated length.
const JPEG_MARKER_READ_BYTES: u64 = 1024;
/// Adobe APP14 payload bytes through the color transform flag.
const ADOBE_APP14_COLOR_BYTES: u64 = 12;
/// SOF payload bytes through the third component, all of a frame header
/// that [`jpeg_sof_color_hint`] reads.
const SOF_COLOR_BYTES: u64 = 6 + 3 * 3;

/// How every stored unit of one TIFF image is encoded and framed.
pub(in super::super) struct UnitCodec {
    pub(in super::super) codec: EncodedCodec,
    pub(in super::super) framing: StoredFraming,
    /// Units per pixel: `SamplesPerPixel` when each sample plane is stored
    /// separately (`PlanarConfiguration` 2), otherwise 1.
    pub(in super::super) planes: u64,
    /// The stored tile grid, for tiled images.
    pub(in super::super) tiled: Option<TiledGeometry>,
    /// Set when the IFD's tags leave each JPEG unit's color to its own
    /// markers; `codec` then describes units whose markers do not decide.
    /// See [`TiffPixelReader::unit_codec`].
    pub(in super::super) marker_color: Option<MarkerColorCodecs>,
}

/// The two codecs a unit's color markers choose between.
pub(in super::super) struct MarkerColorCodecs {
    rgb: EncodedCodec,
    ycbcr: EncodedCodec,
}

#[derive(Clone, Copy)]
pub(in super::super) struct TiledGeometry {
    /// Stored (padded) tile size, `TileWidth` x `TileLength`.
    pub(in super::super) tile_width: u32,
    pub(in super::super) tile_height: u32,
    /// Tiles in one sample plane of the image.
    pub(in super::super) tiles_per_plane: u64,
}

/// Whether a raw tile of `codec`'s stored bytes is fully described by
/// [`RawCompressedTile`]'s compression, bits, samples and photometric
/// interpretation: no predictor, chunky samples in this machine's byte
/// order, and no subsampled YCbCr packing.
pub(in super::super) fn bare_payload_is_self_describing(codec: &UnitCodec) -> bool {
    codec.planes == 1 && bare_codec_is_self_describing(&codec.codec)
}

/// [`bare_payload_is_self_describing`] for one unit's codec.
pub(in super::super) fn bare_codec_is_self_describing(codec: &EncodedCodec) -> bool {
    let native = if cfg!(target_endian = "little") {
        ByteOrder::LittleEndian
    } else {
        ByteOrder::BigEndian
    };
    codec.planar_configuration() == PlanarConfiguration::Chunky
        && codec.predictor() == Predictor::None
        && (codec.bits_per_sample() <= 8 || codec.byte_order() == Some(native))
        && !ycbcr_is_subsampled(codec)
}

/// Whether `codec` stores YCbCr samples in TIFF's subsampled packing, which
/// [`RawCompressedTile`] cannot describe.
pub(in super::super) fn ycbcr_is_subsampled(codec: &EncodedCodec) -> bool {
    codec.photometric_interpretation() == EncodedTilePhotometricInterpretation::YbrFull422
        && codec.chroma_subsampling() != Some((1, 1))
}

impl TiffPixelReader {
    /// The codec, framing and plane layout of `ifd_id`'s stored units.
    /// `jpeg_tables` are the tables the reader decodes the IFD with, which
    /// may be inherited from another level.
    pub(in super::super) fn tiff_unit_codec(
        &self,
        ifd_id: IfdId,
        compression: Compression,
        jpeg_tables: Option<&[u8]>,
    ) -> Result<UnitCodec, WsiError> {
        let invalid = |reason: String| WsiError::Unsupported {
            reason: format!("TIFF IFD {ifd_id} cannot be described as encoded units: {reason}"),
        };
        let tag = |tag| self.container.get_u32(ifd_id, tag).ok();
        let default_samples = match compression {
            Compression::Jpeg | Compression::Jp2kRgb | Compression::Jp2kYcbcr => 3,
            _ => 1,
        };
        let samples = tag(tags::SAMPLES_PER_PIXEL).unwrap_or(default_samples);
        let samples_u16 = u16::try_from(samples)
            .ok()
            .filter(|&samples| samples > 0)
            .ok_or_else(|| invalid(format!("unsupported SamplesPerPixel {samples}")))?;
        let bits = self.uniform_bits_per_sample(ifd_id).map_err(invalid)?;
        let planes = if samples > 1 && tag(tags::PLANAR_CONFIGURATION) == Some(2) {
            u64::from(samples)
        } else {
            1
        };

        let mut marker_color = None;
        let (mut codec, framing) = match compression {
            Compression::Jpeg => {
                if tag(tags::COMPRESSION) == Some(6) {
                    return Err(invalid(
                        "old-style JPEG (compression 6) units are not self-contained JPEG streams"
                            .into(),
                    ));
                }
                let photometric_tag = tag(tags::PHOTOMETRIC);
                let subsampling = if photometric_tag == Some(6) {
                    self.ycbcr_subsampling(ifd_id)
                } else {
                    None
                };
                let tables: Option<Arc<[u8]>> = jpeg_tables.map(Arc::from);
                let jpeg_codec = |photometric| {
                    let mut codec = EncodedCodec::new(compression, bits, samples_u16, photometric);
                    if let Some((horizontal, vertical)) = subsampling {
                        codec = codec.with_chroma_subsampling(horizontal, vertical);
                    }
                    if planes > 1 {
                        codec = codec.with_planar_configuration(PlanarConfiguration::Planar);
                    }
                    match &tables {
                        Some(tables) => codec.with_jpeg_tables(tables.clone()),
                        None => codec,
                    }
                };
                // The decoder takes color markers from the shared tables
                // before the unit's own.
                let tables_hint = match (samples, jpeg_tables) {
                    (3, Some(tables)) => jpeg_bitstream_color_hint(&[], Some(tables)),
                    _ => JpegBitstreamColorHint::Unknown,
                };
                if self.jpeg_color_is_left_to_markers(photometric_tag, samples, planes, tables_hint)
                {
                    marker_color = Some(MarkerColorCodecs {
                        rgb: jpeg_codec(EncodedTilePhotometricInterpretation::Rgb),
                        ycbcr: jpeg_codec(EncodedTilePhotometricInterpretation::YbrFull422),
                    });
                }
                let photometric =
                    self.tiff_jpeg_location_photometric(photometric_tag, samples, tables_hint)?;
                let framing = if tables.is_some() {
                    StoredFraming::JpegTables
                } else {
                    StoredFraming::Standalone
                };
                (jpeg_codec(photometric), framing)
            }
            Compression::Jp2kRgb | Compression::Jp2kYcbcr => {
                let (bits, samples, photometric) =
                    self.tiled_ifd_jp2k_format(ifd_id, compression)?;
                (
                    EncodedCodec::new(compression, bits, samples, photometric),
                    StoredFraming::Standalone,
                )
            }
            _ => {
                self.require_unsigned_samples(ifd_id).map_err(invalid)?;
                let photometric = bare_photometric(tag(tags::PHOTOMETRIC), samples)
                    .map_err(|reason| invalid(reason.into()))?;
                let codec = EncodedCodec::new(compression, bits, samples_u16, photometric);
                if compression == Compression::JpegXr {
                    // Each JPEG XR tile is a complete JPEG XR image.
                    (codec, StoredFraming::Standalone)
                } else {
                    let predictor = match tag(tags::PREDICTOR).unwrap_or(1) {
                        1 => Predictor::None,
                        2 => Predictor::Horizontal,
                        3 => Predictor::FloatingPoint,
                        other => return Err(invalid(format!("unsupported Predictor {other}"))),
                    };
                    let byte_order = match self.container.endian() {
                        crate::formats::tiff_family::container::Endian::Little => {
                            ByteOrder::LittleEndian
                        }
                        crate::formats::tiff_family::container::Endian::Big => ByteOrder::BigEndian,
                    };
                    let mut codec = codec.with_predictor(predictor).with_byte_order(byte_order);
                    if photometric == EncodedTilePhotometricInterpretation::YbrFull422 {
                        // Raw YCbCr samples are packed by their subsampling,
                        // which TIFF defaults to 2x2.
                        let (horizontal, vertical) =
                            self.ycbcr_subsampling(ifd_id).unwrap_or((2, 2));
                        codec = codec.with_chroma_subsampling(horizontal, vertical);
                    }
                    (codec, StoredFraming::BarePayload)
                }
            }
        };
        if planes > 1 {
            codec = codec.with_planar_configuration(PlanarConfiguration::Planar);
        }
        Ok(UnitCodec {
            codec,
            framing,
            planes,
            tiled: self.tiled_geometry(ifd_id, planes).map_err(invalid)?,
            marker_color,
        })
    }

    /// Whether each JPEG unit's color markers decide its photometric
    /// interpretation: three chunky samples under an RGB (or absent)
    /// Photometric tag, which a unit's chroma subsampling or Adobe APP14
    /// marker overrides, or any Philips IFD, whose tags never describe the
    /// color transform. Shared tables that decide the color decide it for
    /// every unit.
    fn jpeg_color_is_left_to_markers(
        &self,
        photometric_tag: Option<u32>,
        samples: u32,
        planes: u64,
        tables_hint: JpegBitstreamColorHint,
    ) -> bool {
        let philips = self.layout.dataset.properties.vendor() == Some("philips");
        samples == 3
            && planes == 1
            && tables_hint == JpegBitstreamColorHint::Unknown
            && (philips || photometric_tag.unwrap_or(2) == 2)
    }

    /// The codec of `codec`'s unit stored at `range`, which must lie inside
    /// the file.
    ///
    /// When [`UnitCodec::marker_color`] is set, the unit's marker segments
    /// are walked to its first Adobe APP14 or SOF segment, as the decoder
    /// reads them: positioned reads of at most [`JPEG_MARKER_READ_BYTES`]
    /// each, one per stretch of short segments and one more per segment too
    /// long for the current read, which is skipped by its stated length.
    /// No read passes the unit's byte count, a typical tile costs a single
    /// 1 KiB read, and nothing is cached per tile. A unit whose color
    /// markers cannot be reached inside its byte count fails rather than
    /// taking the tags' default. Other IFDs read nothing here.
    pub(in super::super) fn unit_codec(
        &self,
        req: &TileRequest,
        codec: &UnitCodec,
        range: ByteRange,
    ) -> Result<EncodedCodec, WsiError> {
        let Some(marker_color) = &codec.marker_color else {
            return Ok(codec.codec.clone());
        };
        let hint = self.jpeg_unit_color_hint(req, range)?;
        Ok(match self.tiff_jpeg_location_photometric(None, 3, hint)? {
            EncodedTilePhotometricInterpretation::Rgb => marker_color.rgb.clone(),
            _ => marker_color.ycbcr.clone(),
        })
    }

    /// The color hint of the JPEG stored at `range`, as
    /// [`jpeg_bitstream_color_hint`] reads the whole stream: its first Adobe
    /// APP14 segment or SOF decides.
    fn jpeg_unit_color_hint(
        &self,
        req: &TileRequest,
        range: ByteRange,
    ) -> Result<JpegBitstreamColorHint, WsiError> {
        let fail = |reason: &str| {
            WsiError::tile_read(
                req,
                format!(
                    "the {}-byte JPEG at file offset {} {reason}, so its color markers \
                     cannot be read",
                    range.length, range.offset
                ),
            )
        };
        let mut window = UnitWindow {
            container: &self.container,
            range,
            start: 0,
            bytes: Vec::new(),
        };
        if window.get(0, 2)? != [0xFF, 0xD8] {
            return Err(fail("does not start with SOI"));
        }
        let mut pos = 2u64;
        loop {
            let head = window.get(pos, 4)?.to_vec();
            let (marker, length) = match head.as_slice() {
                [] => return Err(fail("ends before its frame header")),
                // A fill byte in front of a marker.
                [0xFF, 0xFF, ..] => {
                    pos += 1;
                    continue;
                }
                [0xFF, 0xD0..=0xD7, ..] => {
                    pos += 2;
                    continue;
                }
                [0xFF, marker @ (0x00 | 0xD8 | 0xD9), ..] => {
                    return Err(fail(&format!(
                        "has marker FF{marker:02X} before its frame header"
                    )))
                }
                [0xFF, marker, high, low] => {
                    (*marker, u64::from(u16::from_be_bytes([*high, *low])))
                }
                [0xFF, ..] => return Err(fail("ends inside a marker segment header")),
                _ => return Err(fail("has bytes outside its marker segments")),
            };
            let end = pos + 2 + length;
            if length < 2 || end > range.length {
                return Err(fail(&format!(
                    "has a FF{marker:02X} segment of length {length} that overruns it"
                )));
            }
            if marker == 0xDA {
                return Err(fail("starts a scan before its frame header"));
            }
            let payload_len = length - 2;
            if is_sof_marker(marker) {
                let payload = window.get(pos + 4, payload_len.min(SOF_COLOR_BYTES))?;
                return Ok(jpeg_sof_color_hint(payload));
            }
            if marker == 0xEE && payload_len >= ADOBE_APP14_COLOR_BYTES {
                let payload = window.get(pos + 4, ADOBE_APP14_COLOR_BYTES)?;
                if payload.starts_with(b"Adobe") {
                    return Ok(match payload[11] {
                        0 => JpegBitstreamColorHint::Rgb,
                        1 => JpegBitstreamColorHint::YCbCr,
                        _ => JpegBitstreamColorHint::Unknown,
                    });
                }
            }
            pos = end;
        }
    }

    /// The photometric interpretation a TIFF JPEG unit decodes with, from
    /// its color markers' `hint` and the container tags, as raw reads
    /// resolve it.
    fn tiff_jpeg_location_photometric(
        &self,
        photometric: Option<u32>,
        samples: u32,
        hint: JpegBitstreamColorHint,
    ) -> Result<EncodedTilePhotometricInterpretation, WsiError> {
        if self.layout.dataset.properties.vendor() == Some("philips") {
            // Philips tags do not describe the JPEG color transform; its
            // decoder follows the bitstream, which defaults to YCbCr.
            return Ok(match (samples, hint) {
                (1, _) => EncodedTilePhotometricInterpretation::Monochrome2,
                (_, JpegBitstreamColorHint::Rgb) => EncodedTilePhotometricInterpretation::Rgb,
                _ => EncodedTilePhotometricInterpretation::YbrFull422,
            });
        }
        tiff_jpeg_encoded_photometric_interpretation(photometric.unwrap_or(2), samples, hint)
    }

    /// `BitsPerSample`, which must be equal for every sample; 8 when absent.
    fn uniform_bits_per_sample(&self, ifd_id: IfdId) -> Result<u16, String> {
        let bits = match self.container.get_u64_array(ifd_id, tags::BITS_PER_SAMPLE) {
            Ok(values) => values,
            Err(_) => return Ok(8),
        };
        let Some(&first) = bits.first() else {
            return Ok(8);
        };
        if bits.iter().any(|&value| value != first) {
            return Err(format!("mixed BitsPerSample {bits:?}"));
        }
        u16::try_from(first)
            .ok()
            .filter(|&bits| bits > 0)
            .ok_or_else(|| format!("unsupported BitsPerSample {first}"))
    }

    /// Fails for signed or floating-point samples, which [`EncodedCodec`]
    /// cannot describe.
    fn require_unsigned_samples(&self, ifd_id: IfdId) -> Result<(), String> {
        match self.container.get_u64_array(ifd_id, tags::SAMPLE_FORMAT) {
            Ok(formats) if formats.iter().any(|&format| format != 1) => Err(format!(
                "SampleFormat {formats:?} is not unsigned integer, which encoded codec \
                 descriptions cannot state"
            )),
            _ => Ok(()),
        }
    }

    fn ycbcr_subsampling(&self, ifd_id: IfdId) -> Option<(u8, u8)> {
        let values = self
            .container
            .get_u64_array(ifd_id, tags::YCBCR_SUBSAMPLING)
            .ok()?;
        match values {
            [horizontal, vertical] => Some((
                u8::try_from(*horizontal).ok()?,
                u8::try_from(*vertical).ok()?,
            )),
            _ => None,
        }
    }

    /// The stored tile grid of a tiled IFD; `None` for strip images.
    fn tiled_geometry(&self, ifd_id: IfdId, planes: u64) -> Result<Option<TiledGeometry>, String> {
        let (Ok(tile_width), Ok(tile_height)) = (
            self.container.get_u32(ifd_id, tags::TILE_WIDTH),
            self.container.get_u32(ifd_id, tags::TILE_LENGTH),
        ) else {
            return Ok(None);
        };
        if tile_width == 0 || tile_height == 0 {
            return Err(format!("empty TIFF tile size {tile_width}x{tile_height}"));
        }
        let dimension = |tag, name| {
            self.container
                .get_u64(ifd_id, tag)
                .map_err(|err| format!("failed to read TIFF {name}: {err}"))
        };
        let width = dimension(tags::IMAGE_WIDTH, "image width")?;
        let height = dimension(tags::IMAGE_LENGTH, "image height")?;
        let tiles_per_plane = width
            .div_ceil(u64::from(tile_width))
            .checked_mul(height.div_ceil(u64::from(tile_height)))
            .filter(|tiles| planes == 1 || tiles.checked_mul(planes).is_some())
            .ok_or_else(|| "TIFF tile count overflows u64".to_string())?;
        Ok(Some(TiledGeometry {
            tile_width,
            tile_height,
            tiles_per_plane,
        }))
    }
}

/// Positioned reads of one stored unit, keeping the last one.
struct UnitWindow<'a> {
    container: &'a TiffContainer,
    range: ByteRange,
    /// Unit offset of `bytes`.
    start: u64,
    bytes: Vec<u8>,
}

impl UnitWindow<'_> {
    /// Up to `len` bytes at unit offset `at`, fewer where the unit ends. A
    /// span outside the last read is read anew, at least
    /// [`JPEG_MARKER_READ_BYTES`] of it where the unit has them.
    fn get(&mut self, at: u64, len: u64) -> Result<&[u8], WsiError> {
        let len = len.min(self.range.length.saturating_sub(at));
        if len == 0 {
            return Ok(&[]);
        }
        let held = self.start + self.bytes.len() as u64;
        if at < self.start || at + len > held {
            let read = len.max(JPEG_MARKER_READ_BYTES).min(self.range.length - at);
            self.bytes = self
                .container
                .pread(self.range.offset + at, read)
                .map_err(|err| err.into_wsi_error(self.container.path()))?;
            self.start = at;
        }
        let from = (at - self.start) as usize;
        Ok(&self.bytes[from..from + len as usize])
    }
}

/// The photometric interpretation of uncompressed-style samples (bare
/// payloads and JPEG XR), as the TIFF raw-sample decoder reads them.
fn bare_photometric(
    photometric: Option<u32>,
    samples: u32,
) -> Result<EncodedTilePhotometricInterpretation, &'static str> {
    match (
        samples,
        photometric.unwrap_or(if samples == 1 { 1 } else { 2 }),
    ) {
        // A single sample under an RGB tag is read as grayscale.
        (1, 1 | 2) => Ok(EncodedTilePhotometricInterpretation::Monochrome2),
        (1, 0) => Ok(EncodedTilePhotometricInterpretation::Monochrome1),
        (3, 2) => Ok(EncodedTilePhotometricInterpretation::Rgb),
        (3, 6) => Ok(EncodedTilePhotometricInterpretation::YbrFull422),
        _ => Err("only grayscale, RGB and YCbCr samples can be described"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_photometric_maps_tiff_photometric_tags() {
        use EncodedTilePhotometricInterpretation::{Monochrome1, Monochrome2, Rgb, YbrFull422};
        assert_eq!(bare_photometric(Some(0), 1), Ok(Monochrome1));
        assert_eq!(bare_photometric(Some(1), 1), Ok(Monochrome2));
        assert_eq!(bare_photometric(None, 1), Ok(Monochrome2));
        assert_eq!(bare_photometric(None, 3), Ok(Rgb));
        assert_eq!(bare_photometric(Some(6), 3), Ok(YbrFull422));
        assert!(bare_photometric(Some(3), 1).is_err());
    }
}
