use std::borrow::Cow;
use std::sync::Arc;

use j2k_core::BackendRequest;

use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout};
use crate::decode::jp2k::{decode_batch_jp2k, Jp2kDecodeJob};
use crate::decode::jpeg::JpegDecodeJob;
use crate::error::WsiError;

use super::DicomImage;
use crate::formats::dicom::decode::{
    decode_dicom_jp2k_u16, decode_dicom_jpeg_batch, decode_rle_lossless_frame,
    dicom_jpeg_color_transform, interleaved_rgb_samples, jp2k_photometric_is_ycbcr,
    validate_jpeg_transfer_syntax_frame,
};
use crate::formats::dicom::metadata::DicomBitDepth;
use crate::formats::dicom::transfer_syntax::DicomTransferSyntax;

impl DicomImage {
    pub(in super::super) fn decode_uncompressed_frame_sample_buffer(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
    ) -> Result<CpuTile, WsiError> {
        let frame = self.read_native_frame(frame_index, level, col, row)?;
        let planar_configuration = self.planar_configuration.unwrap_or(0);
        // Interleaved 8-bit RGB frames pass through without a copy.
        let data = match self.bit_depth {
            DicomBitDepth::Eight => interleaved_rgb_samples(
                frame,
                self.tile_width,
                self.tile_height,
                self.samples_per_pixel,
                planar_configuration,
                &self.photometric_interpretation,
            )
            .map(CpuTileData::u8),
            DicomBitDepth::Uint16 { .. } => {
                let mask = self.bit_depth.unsigned_sample_mask();
                let from_bytes =
                    if self.transfer_syntax == (DicomTransferSyntax::Native { big_endian: true }) {
                        u16::from_be_bytes
                    } else {
                        u16::from_le_bytes
                    };
                let words = frame
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|bytes| from_bytes(*bytes) & mask)
                    .collect::<Vec<_>>();
                interleaved_rgb_samples(
                    words,
                    self.tile_width,
                    self.tile_height,
                    self.samples_per_pixel,
                    planar_configuration,
                    &self.photometric_interpretation,
                )
                .map(CpuTileData::u16)
            }
        };
        data.and_then(|data| {
            CpuTile::new(
                self.tile_width,
                self.tile_height,
                3,
                ColorSpace::Rgb,
                CpuTileLayout::Interleaved,
                data,
            )
        })
        .map_err(|err| WsiError::TileRead {
            col,
            row,
            level,
            reason: err.to_string(),
        })
    }

    /// The stored bytes of native frame `frame_index`, once the frame fits
    /// the decoded-image limit and the Pixel Data value.
    fn read_native_frame(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
    ) -> Result<Vec<u8>, WsiError> {
        let tile_read = |reason: String| WsiError::TileRead {
            col,
            row,
            level,
            reason,
        };
        let frame_len = crate::core::limits::checked_product_to_usize(
            &[
                u64::from(self.tile_width),
                u64::from(self.tile_height),
                u64::from(self.samples_per_pixel),
                u64::from(self.bit_depth.bytes_per_sample()),
            ],
            crate::core::limits::MAX_DECODED_IMAGE_BYTES,
            "native DICOM frame",
        )
        .map_err(tile_read)?;
        let native_pixel_data =
            self.frame_store.native_pixel_data.as_ref().ok_or_else(|| {
                tile_read("native DICOM Pixel Data location is unavailable".into())
            })?;
        let range = self.native_frame_range(frame_index).map_err(tile_read)?;
        let mut frame = Vec::new();
        frame
            .try_reserve_exact(frame_len)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "native DICOM frame",
                requested: frame_len as u64,
                limit: crate::core::limits::MAX_DECODED_IMAGE_BYTES,
            })?;
        frame.resize(frame_len, 0);
        native_pixel_data.source_identity.ensure_matches(
            &self.frame_store.path,
            "DICOM source changed after metadata was parsed",
        )?;
        native_pixel_data
            .source_file
            .read_exact_at(&mut frame, range.offset)
            .map_err(|source| WsiError::io_with_path(&self.frame_store.path, source))?;
        Ok(frame)
    }

    /// A native frame as a raw uncompressed tile: interleaved samples, and
    /// 16-bit samples in host byte order with the bits above `BitsStored`
    /// cleared. Frames already stored that way pass through unchanged;
    /// others are reordered and masked as `read_tile` reads them.
    pub(in super::super) fn read_native_raw_frame(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
    ) -> Result<Vec<u8>, WsiError> {
        let mut frame = self.read_native_frame(frame_index, level, col, row)?;
        let sample_bytes = usize::from(self.bit_depth.bytes_per_sample());
        if sample_bytes == 2 {
            let from_stored =
                if self.transfer_syntax == (DicomTransferSyntax::Native { big_endian: true }) {
                    u16::from_be_bytes
                } else {
                    u16::from_le_bytes
                };
            // Bits above BitsStored are not part of the sample; read_tile
            // clears them too.
            let mask = self.bit_depth.unsigned_sample_mask();
            for word in frame.as_chunks_mut::<2>().0 {
                *word = (from_stored(*word) & mask).to_ne_bytes();
            }
        }
        if self.samples_per_pixel > 1 && self.planar_configuration == Some(1) {
            frame =
                interleave_sample_planes(&frame, usize::from(self.samples_per_pixel), sample_bytes);
        }
        Ok(frame)
    }

    /// Checks that a JPEG frame uses this image's declared JPEG process and
    /// sample precision.
    pub(in super::super) fn validate_jpeg_frame(&self, frame: &[u8]) -> Result<(), WsiError> {
        validate_jpeg_transfer_syntax_frame(
            self.transfer_syntax,
            &self.transfer_syntax_uid,
            self.bit_depth,
            frame,
        )
    }

    /// Decode a JPEG 2000 frame of this image's 16-bit words to RGB16.
    pub(in super::super) fn decode_jp2k_u16_frame(
        &self,
        bytes: &[u8],
        parallelism: j2k::CpuDecodeParallelism,
    ) -> Result<CpuTile, WsiError> {
        decode_dicom_jp2k_u16(
            bytes,
            self.tile_width,
            self.tile_height,
            self.samples_per_pixel,
            &self.photometric_interpretation,
            self.bit_depth.bits_stored(),
            parallelism,
        )
    }

    /// The JPEG decode job for one of this image's frames, once the frame
    /// matches the declared process and precision.
    pub(in super::super) fn jpeg_job<'a>(
        &self,
        bytes: &'a [u8],
    ) -> Result<JpegDecodeJob<'a>, WsiError> {
        self.validate_jpeg_frame(bytes)?;
        Ok(JpegDecodeJob {
            data: Cow::Borrowed(bytes),
            tables: None,
            expected_width: self.tile_width,
            expected_height: self.tile_height,
            color_transform: dicom_jpeg_color_transform(&self.photometric_interpretation),
            force_dimensions: false,
            requested_size: None,
        })
    }

    /// The decode job for one of this image's 8-bit JPEG 2000 frames.
    pub(in super::super) fn jp2k_job<'a>(
        &self,
        bytes: &'a [u8],
        backend: BackendRequest,
    ) -> Jp2kDecodeJob<'a> {
        Jp2kDecodeJob {
            data: Cow::Borrowed(bytes),
            expected_width: self.tile_width,
            expected_height: self.tile_height,
            rgb_color_space: !jp2k_photometric_is_ycbcr(&self.photometric_interpretation),
            backend,
        }
    }

    /// Decode one of this image's RLE Lossless frames.
    pub(in super::super) fn decode_rle(&self, bytes: &[u8]) -> Result<CpuTile, WsiError> {
        decode_rle_lossless_frame(
            bytes,
            self.tile_width,
            self.tile_height,
            self.samples_per_pixel,
            &self.photometric_interpretation,
            self.bit_depth,
        )
    }

    pub(in super::super) fn decode_frame_sample_buffer(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let use_decoded_cache = self.transfer_syntax.is_encapsulated();
        if use_decoded_cache {
            if let Some(cached) = self.cached_decoded_frame(frame_index) {
                return Ok(cached.as_ref().clone());
            }
        }

        let extract = || self.extract_encapsulated_frame(frame_index, level, col, row, false);
        let tile_read = |err: WsiError| WsiError::TileRead {
            col,
            row,
            level,
            reason: err.to_string(),
        };
        let buffer = match self.transfer_syntax {
            DicomTransferSyntax::Jpeg(_) => {
                let bytes = extract()?;
                let job = self.jpeg_job(bytes.as_slice()).map_err(tile_read)?;
                crate::core::batch::exactly_one(
                    decode_dicom_jpeg_batch(self.bit_depth, &[job]),
                    "DICOM JPEG frame decode",
                )?
                .map_err(tile_read)?
            }
            DicomTransferSyntax::Jp2k { .. } => {
                let bytes = extract()?;
                match self.bit_depth {
                    DicomBitDepth::Eight => crate::core::batch::exactly_one(
                        decode_batch_jp2k(&[self.jp2k_job(bytes.as_slice(), backend)]),
                        "DICOM JP2K frame decode",
                    )?,
                    DicomBitDepth::Uint16 { .. } => crate::core::batch::install_codec_cpu(|| {
                        self.decode_jp2k_u16_frame(
                            bytes.as_slice(),
                            j2k::CpuDecodeParallelism::Auto,
                        )
                    }),
                }
                .map_err(tile_read)?
            }
            DicomTransferSyntax::Rle => {
                self.decode_rle(extract()?.as_slice()).map_err(tile_read)?
            }
            DicomTransferSyntax::Native { .. } => {
                self.decode_uncompressed_frame_sample_buffer(frame_index, level, col, row)?
            }
        };

        if use_decoded_cache {
            self.cache_decoded_frame(frame_index, Arc::new(buffer.clone()));
        }
        Ok(buffer)
    }
}

/// Interleaves `planes` equal sample planes of `sample_bytes`-byte samples.
fn interleave_sample_planes(planar: &[u8], planes: usize, sample_bytes: usize) -> Vec<u8> {
    let pixel_count = planar.len() / (planes * sample_bytes);
    let mut interleaved = vec![0; pixel_count * planes * sample_bytes];
    for (plane, plane_bytes) in planar.chunks_exact(pixel_count * sample_bytes).enumerate() {
        for (pixel, sample) in plane_bytes.chunks_exact(sample_bytes).enumerate() {
            let start = (pixel * planes + plane) * sample_bytes;
            interleaved[start..start + sample_bytes].copy_from_slice(sample);
        }
    }
    interleaved
}
