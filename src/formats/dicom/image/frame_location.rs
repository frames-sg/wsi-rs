//! Where each frame's encoded bytes are stored, from the frame index.

use crate::core::positioned_file::PositionedFile;
use crate::core::types::{
    ByteOrder, ByteRange, EncodedCodec, EncodedUnit, PixelRect, PlanarConfiguration, StoredFraming,
    StoredTile, TileLocation,
};
use crate::error::WsiError;

use super::DicomImage;
use crate::formats::dicom::decode::raw_photometric_interpretation;
use crate::formats::dicom::transfer_syntax::DicomTransferSyntax;

impl DicomImage {
    /// Where the frame of tile (`col`, `row`) is stored.
    ///
    /// An encapsulated frame is the value bytes of its fragments, in order,
    /// without Item headers. JPEG and JPEG 2000 frames are standalone
    /// codestreams; the last segment leaves out the zero byte DICOM pads an
    /// odd-length stream with after its `FF D9` marker, exactly as
    /// [`DicomImage::read_raw_compressed_tile`] trims it. Telling that byte
    /// from the stream takes a read of the frame's last three bytes, the only
    /// frame bytes a location reads. RLE frames, header and padding included,
    /// and native frames are bare payloads that need the codec's size, sample
    /// and layout parameters. Sparse-grid tiles without a frame are absent.
    pub(in super::super) fn tile_location(
        &self,
        col: u32,
        row: u32,
    ) -> Result<TileLocation, WsiError> {
        let Some(frame_index) = self.frame_index(col, row) else {
            return Ok(TileLocation::Absent);
        };
        let (width, height) = self.actual_tile_dimensions(col, row);
        let (segments, framing) = match self.transfer_syntax {
            DicomTransferSyntax::Native { .. } => (
                vec![self
                    .native_frame_range(frame_index)
                    .map_err(|reason| self.invalid_location(frame_index, reason))?],
                StoredFraming::BarePayload,
            ),
            DicomTransferSyntax::Jpeg(_) | DicomTransferSyntax::Jp2k { .. } => (
                self.fragment_segments(frame_index)?,
                StoredFraming::Standalone,
            ),
            DicomTransferSyntax::Rle => (
                self.fragment_segments(frame_index)?,
                StoredFraming::BarePayload,
            ),
        };
        let whole = PixelRect::new(0, 0, width, height);
        let unit = EncodedUnit::builder(
            self.frame_store.source_path.clone(),
            self.location_codec()?,
            self.tile_width,
            self.tile_height,
        )
        .segments(segments)
        .framing(framing)
        .source_region(whole)
        .tile_region(whole)
        .build(self.frame_store.source_len)?;
        Ok(TileLocation::Stored(StoredTile::new(
            width,
            height,
            vec![unit],
        )?))
    }

    /// The absolute file range of native frame `frame_index`, inside the
    /// Pixel Data value.
    pub(in super::super) fn native_frame_range(
        &self,
        frame_index: u32,
    ) -> Result<ByteRange, String> {
        let native = self
            .frame_store
            .native_pixel_data
            .as_ref()
            .ok_or("native DICOM Pixel Data location is unavailable")?;
        let frame_len = self
            .checked_native_frame_bytes()
            .ok_or("DICOM frame size overflows u64")?;
        let start = u64::from(frame_index)
            .checked_mul(frame_len)
            .ok_or("DICOM frame offset overflow")?;
        let end = start
            .checked_add(frame_len)
            .ok_or("DICOM frame byte range overflow")?;
        if end > u64::from(native.value_len) {
            return Err(format!(
                "DICOM frame {frame_index} byte range {start}..{end} exceeds pixel data length {}",
                native.value_len
            ));
        }
        let offset = native
            .value_offset
            .checked_add(start)
            .ok_or("DICOM frame file offset overflow")?;
        Ok(ByteRange::new(offset, frame_len))
    }

    /// The value bytes of each fragment of encapsulated frame `frame_index`.
    /// Empty fragments hold no bytes of the stream and are left out.
    fn fragment_segments(&self, frame_index: u32) -> Result<Vec<ByteRange>, WsiError> {
        let frames = self.ensure_encapsulated_frames()?;
        let fragments = frames
            .frame_ranges
            .get(frame_index as usize)
            .and_then(|range| frames.fragments.get(range.clone()))
            .ok_or_else(|| {
                self.invalid_location(
                    frame_index,
                    format!(
                        "frame is outside the {}-frame index",
                        frames.frame_ranges.len()
                    ),
                )
            })?;
        let mut segments = fragments
            .iter()
            .filter(|fragment| fragment.len > 0)
            .map(|fragment| ByteRange::new(fragment.payload_offset, u64::from(fragment.len)))
            .collect::<Vec<_>>();
        if self.transfer_syntax != DicomTransferSyntax::Rle && self.ends_with_padding(&segments)? {
            if let Some(last) = segments.last_mut() {
                last.length -= 1;
                if last.length == 0 {
                    segments.pop();
                }
            }
        }
        Ok(segments)
    }

    /// Whether the stream in `segments` ends with the padding byte that
    /// `trim_encapsulated_frame_padding` removes: a zero after `FF D9`.
    fn ends_with_padding(&self, segments: &[ByteRange]) -> Result<bool, WsiError> {
        let mut tail = [0u8; 3];
        let mut needed = tail.len();
        for range in segments.iter().rev() {
            if needed == 0 {
                break;
            }
            let take = needed.min(usize::try_from(range.length).unwrap_or(usize::MAX));
            let end = range
                .end()
                .filter(|end| *end <= self.frame_store.source_len)
                .ok_or_else(|| {
                    WsiError::invalid_slide(
                        &self.frame_store.path,
                        format!(
                            "DICOM fragment {}+{} exceeds the {}-byte file",
                            range.offset, range.length, self.frame_store.source_len
                        ),
                    )
                })?;
            self.location_file()?
                .read_exact_at(&mut tail[needed - take..needed], end - take as u64)
                .map_err(|source| WsiError::io_with_path(&self.frame_store.path, source))?;
            needed -= take;
        }
        Ok(needed == 0 && tail == [0xFF, 0xD9, 0x00])
    }

    fn location_file(&self) -> Result<&PositionedFile, WsiError> {
        if let Some(file) = self.frame_store.location_file.get() {
            return Ok(file);
        }
        let file = std::fs::File::open(&self.frame_store.path)
            .map_err(|source| WsiError::io_with_path(&self.frame_store.path, source))?;
        Ok(self
            .frame_store
            .location_file
            .get_or_init(|| PositionedFile::new(file)))
    }

    /// How this image's frames are encoded. Samples in 16-bit words report
    /// `BitsStored` as their significant bits when it is fewer than 16.
    fn location_codec(&self) -> Result<EncodedCodec, WsiError> {
        let mut codec = EncodedCodec::new(
            self.transfer_syntax
                .source_compression(&self.photometric_interpretation),
            self.bit_depth.bits_allocated(),
            self.samples_per_pixel,
            raw_photometric_interpretation(
                self.samples_per_pixel,
                &self.photometric_interpretation,
            )?,
        );
        let bits_stored = u16::from(self.bit_depth.bits_stored());
        if bits_stored < self.bit_depth.bits_allocated() {
            codec = codec.with_significant_bits(bits_stored);
        }
        if let DicomTransferSyntax::Native { big_endian } = self.transfer_syntax {
            if self.samples_per_pixel > 1 && self.planar_configuration == Some(1) {
                codec = codec.with_planar_configuration(PlanarConfiguration::Planar);
            }
            if self.bit_depth.bytes_per_sample() > 1 {
                codec = codec.with_byte_order(if big_endian {
                    ByteOrder::BigEndian
                } else {
                    ByteOrder::LittleEndian
                });
            }
        }
        Ok(codec)
    }

    fn invalid_location(&self, frame_index: u32, reason: impl std::fmt::Display) -> WsiError {
        WsiError::invalid_slide(
            &self.frame_store.path,
            format!("DICOM frame {frame_index} location: {reason}"),
        )
    }
}
