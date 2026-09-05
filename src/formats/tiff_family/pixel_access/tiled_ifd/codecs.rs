use super::super::*;

fn codec_error(
    codec: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> WsiError {
    WsiError::Codec {
        codec,
        source: Box::new(source),
    }
}

fn tiff_lzw_error(kind: std::io::ErrorKind, message: impl Into<String>) -> WsiError {
    codec_error("tiff-lzw", std::io::Error::new(kind, message.into()))
}

fn decode_tiff_lzw(input: &[u8], out: &mut [u8]) -> Result<usize, WsiError> {
    let scratch_len = out
        .len()
        .checked_add(1)
        .ok_or_else(|| WsiError::UnsupportedFormat("TIFF LZW output length overflow".into()))?;
    let mut scratch = vec![0u8; scratch_len];
    let mut decoder = weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
    let mut input_offset = 0usize;
    let mut output_offset = 0usize;

    loop {
        if output_offset == scratch.len() {
            return Err(tiff_lzw_error(
                std::io::ErrorKind::InvalidData,
                "decoded data exceeds expected TIFF strip size",
            ));
        }
        let result = decoder.decode_bytes(&input[input_offset..], &mut scratch[output_offset..]);
        input_offset += result.consumed_in;
        output_offset += result.consumed_out;
        if output_offset > out.len() {
            return Err(tiff_lzw_error(
                std::io::ErrorKind::InvalidData,
                "decoded data exceeds expected TIFF strip size",
            ));
        }

        match result.status {
            Ok(weezl::LzwStatus::Done) => {
                out[..output_offset].copy_from_slice(&scratch[..output_offset]);
                return Ok(output_offset);
            }
            Ok(weezl::LzwStatus::Ok) => {
                if input_offset == input.len() {
                    return Err(tiff_lzw_error(
                        std::io::ErrorKind::UnexpectedEof,
                        "truncated TIFF LZW strip",
                    ));
                }
            }
            Ok(weezl::LzwStatus::NoProgress) => {
                return Err(tiff_lzw_error(
                    std::io::ErrorKind::InvalidData,
                    "TIFF LZW decoder made no progress",
                ));
            }
            Err(error) => {
                return Err(tiff_lzw_error(
                    std::io::ErrorKind::InvalidData,
                    format!("TIFF LZW decoder error: {error:?}"),
                ));
            }
        }
    }
}

impl TiffPixelReader {
    pub(in super::super) fn tiff_jpeg_decode_options_for_data(
        &self,
        ifd_id: IfdId,
        force_dimensions: bool,
        data: &[u8],
        tables: Option<&[u8]>,
    ) -> TiffJpegDecodeOptions {
        self.tiff_jpeg_decode_options_with_hint(
            ifd_id,
            force_dimensions,
            jpeg_bitstream_color_hint(data, tables),
        )
    }

    pub(in super::super) fn tiff_jpeg_decode_options_with_hint(
        &self,
        ifd_id: IfdId,
        force_dimensions: bool,
        bitstream_hint: JpegBitstreamColorHint,
    ) -> TiffJpegDecodeOptions {
        if self.layout.dataset.properties.vendor() == Some("philips") {
            return TiffJpegDecodeOptions {
                force_dimensions,
                color_transform: J2kColorTransform::Auto,
            };
        }

        let photometric = self
            .container
            .get_u32(ifd_id, tags::PHOTOMETRIC)
            .unwrap_or(2);
        let samples_per_pixel = self
            .container
            .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(3);
        let color_transform =
            tiff_jpeg_color_transform(photometric, samples_per_pixel, bitstream_hint);
        TiffJpegDecodeOptions {
            force_dimensions,
            color_transform,
        }
    }

    pub(in super::super) fn decode_tiled_ifd_jpeg_tile_data(
        &self,
        ifd_id: IfdId,
        jpeg_tables: Option<&[u8]>,
        tile_data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<CpuTile, WsiError> {
        let options = self.tiff_jpeg_decode_options_for_data(ifd_id, false, tile_data, jpeg_tables);
        decode_one_jpeg(JpegDecodeJob {
            data: Cow::Borrowed(tile_data),
            tables: jpeg_tables.map(Cow::Borrowed),
            expected_width: width,
            expected_height: height,
            color_transform: options.color_transform,
            force_dimensions: options.force_dimensions,
            requested_size: None,
        })
    }

    pub(in super::super) fn empty_rgb_tile(width: u32, height: u32) -> Result<CpuTile, WsiError> {
        let pixel_count = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(|| {
                WsiError::UnsupportedFormat(format!(
                    "empty RGB tile dimensions {}x{} overflow output buffer size",
                    width, height
                ))
            })?;
        Ok(CpuTile {
            width,
            height,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(vec![0u8; pixel_count]),
        })
    }

    pub(in super::super) fn empty_transparent_tile(
        width: u32,
        height: u32,
    ) -> Result<CpuTile, WsiError> {
        let byte_len = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| {
                WsiError::UnsupportedFormat(format!(
                    "empty RGBA tile dimensions {width}x{height} overflow output buffer size"
                ))
            })?;
        CpuTile::from_u8_interleaved(width, height, 4, ColorSpace::Rgba, vec![0; byte_len])
    }

    /// Decode an uncompressed TIFF tile using IFD metadata.
    pub(in super::super) fn decode_uncompressed_tile(
        &self,
        ifd_id: IfdId,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<CpuTile, WsiError> {
        use crate::formats::tiff_family::container::Endian;

        // Resolve TIFF metadata from container
        let spp = self
            .container
            .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(1);
        let bps_val = self
            .container
            .get_u32(ifd_id, tags::BITS_PER_SAMPLE)
            .unwrap_or(8);
        // Tag 339 SAMPLE_FORMAT: 1=unsigned int (default), 2=signed int, 3=float
        let sample_format = self.container.get_u32(ifd_id, 339).unwrap_or(1);
        // Tag 262 PHOTOMETRIC: 0=MinIsWhite, 1=MinIsBlack, 2=RGB, 3=Palette, 6=YCbCr.
        // When the tag is absent, prefer grayscale for single-sample images and
        // RGB otherwise. Real NDPI associated thumbnails omit PHOTOMETRIC while
        // still storing 8-bit grayscale strips.
        let photometric = self
            .container
            .get_u32(ifd_id, tags::PHOTOMETRIC)
            .unwrap_or(if spp == 1 { 1 } else { 2 });
        // Tag 284 PLANAR_CONFIGURATION: 1=chunky (default), 2=planar
        let planar = self.container.get_u32(ifd_id, 284).unwrap_or(1);

        let endian = self.container.endian();

        if planar == 2 {
            return Err(WsiError::UnsupportedFormat(
                "planar TIFF tiles not supported".into(),
            ));
        }

        let effective_photometric = if spp == 1 && photometric == 2 {
            1
        } else {
            photometric
        };

        // Determine sample type and color space. Some NDPI associated images
        // report RGB photometric with a single 8-bit sample plane; treat those
        // contradictory tags as grayscale because the byte layout is 1 channel.
        let (sample_type, color_space) = match (bps_val, sample_format, spp, effective_photometric)
        {
            (8, 1, 3, 2) => (SampleType::Uint8, ColorSpace::Rgb), // RGB u8
            (8, 1, 1, 0) => (SampleType::Uint8, ColorSpace::Grayscale), // MinIsWhite (inverted below)
            (8, 1, 1, 1) => (SampleType::Uint8, ColorSpace::Grayscale), // MinIsBlack
            (8, 1, 3, 6) => (SampleType::Uint8, ColorSpace::YCbCr),     // YCbCr u8
            (16, 1, 1, 0) | (16, 1, 1, 1) => (SampleType::Uint16, ColorSpace::Grayscale),
            (16, 1, 3, 2) => (SampleType::Uint16, ColorSpace::Rgb), // RGB u16
            (32, 3, 1, _) => (SampleType::Float32, ColorSpace::Grayscale), // Float32 grayscale
            _ => {
                return Err(WsiError::UnsupportedFormat(format!(
                    "unsupported uncompressed format: bps={}, format={}, spp={}, photometric={}",
                    bps_val, sample_format, spp, photometric,
                )));
            }
        };

        let expected_bytes = crate::core::limits::checked_product_to_usize(
            &[
                u64::from(width),
                u64::from(height),
                u64::from(spp),
                sample_type.byte_size() as u64,
            ],
            crate::core::limits::MAX_DECODED_IMAGE_BYTES,
            "uncompressed TIFF tile",
        )
        .map_err(WsiError::DisplayConversion)?;
        if data.len() < expected_bytes {
            return Err(WsiError::TileRead {
                col: 0,
                row: 0,
                level: 0u32,
                reason: format!(
                    "uncompressed tile data too short: {} < {}",
                    data.len(),
                    expected_bytes,
                ),
            });
        }

        let sample_data = match sample_type {
            SampleType::Uint8 => {
                let mut bytes = data[..expected_bytes].to_vec();
                // MinIsWhite: invert grayscale values
                if effective_photometric == 0 {
                    for b in &mut bytes {
                        *b = 255 - *b;
                    }
                }
                CpuTileData::u8(bytes)
            }
            SampleType::Uint16 => {
                let mut samples: Vec<u16> = data[..expected_bytes]
                    .chunks_exact(2)
                    .map(|c| match endian {
                        Endian::Little => u16::from_le_bytes([c[0], c[1]]),
                        Endian::Big => u16::from_be_bytes([c[0], c[1]]),
                    })
                    .collect();
                // MinIsWhite: invert
                if effective_photometric == 0 {
                    for s in &mut samples {
                        *s = u16::MAX - *s;
                    }
                }
                CpuTileData::u16(samples)
            }
            SampleType::Float32 => {
                let samples: Vec<f32> = data[..expected_bytes]
                    .chunks_exact(4)
                    .map(|c| match endian {
                        Endian::Little => f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                        Endian::Big => f32::from_be_bytes([c[0], c[1], c[2], c[3]]),
                    })
                    .collect();
                CpuTileData::f32(samples)
            }
        };

        // After MinIsWhite inversion, report as standard Grayscale
        // (the inversion already happened in the sample data)
        if effective_photometric == 0 && color_space == ColorSpace::Grayscale {
            // Already inverted above — color_space stays Grayscale
        }

        Ok(CpuTile {
            width,
            height,
            channels: spp as u16,
            color_space,
            layout: CpuTileLayout::Interleaved,
            data: sample_data,
        })
    }

    pub(in super::super) fn expected_uncompressed_tile_bytes(
        &self,
        ifd_id: IfdId,
        width: u32,
        height: u32,
    ) -> Result<usize, WsiError> {
        let spp = self
            .container
            .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(1);
        let bps = self
            .container
            .get_u32(ifd_id, tags::BITS_PER_SAMPLE)
            .unwrap_or(8);
        if bps == 0 || !bps.is_multiple_of(8) {
            return Err(WsiError::UnsupportedFormat(format!(
                "unsupported compressed TIFF bits per sample: {bps}"
            )));
        }
        checked_product_to_usize(
            &[
                u64::from(width),
                u64::from(height),
                u64::from(spp),
                u64::from(bps / 8),
            ],
            MAX_DECODED_IMAGE_BYTES,
            "compressed TIFF tile",
        )
        .map_err(WsiError::UnsupportedFormat)
    }

    pub(in super::super) fn decompress_tiff_payload(
        &self,
        ifd_id: IfdId,
        compression: Compression,
        input: &[u8],
        expected_bytes: usize,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, WsiError> {
        let mut out = vec![0_u8; expected_bytes];
        let written = match compression {
            Compression::Lzw => decode_tiff_lzw(input, &mut out)?,
            Compression::Deflate => {
                let mut pool = DeflatePool::new();
                DeflateCodec::decompress_into(&mut pool, input, &mut out)
                    .map_err(|error| codec_error("tiff-deflate", error))?
            }
            Compression::Zstd => {
                let mut pool = ZstdPool::new();
                ZstdCodec::decompress_into(&mut pool, input, &mut out)
                    .map_err(|error| codec_error("tiff-zstd", error))?
            }
            other => {
                return Err(WsiError::UnsupportedFormat(format!(
                    "compression {:?} is not a tilecodec payload",
                    other
                )));
            }
        };
        if written != expected_bytes {
            return Err(WsiError::UnsupportedFormat(format!(
                "decoded TIFF payload produced {written} bytes for a {expected_bytes}-byte tile"
            )));
        }
        self.apply_tiff_predictor(ifd_id, width, height, &mut out)?;
        Ok(out)
    }

    pub(in super::super) fn apply_tiff_predictor(
        &self,
        ifd_id: IfdId,
        width: u32,
        height: u32,
        data: &mut [u8],
    ) -> Result<(), WsiError> {
        use crate::formats::tiff_family::container::Endian;

        let predictor = self.container.get_u32(ifd_id, tags::PREDICTOR).unwrap_or(1);
        if predictor == 1 {
            return Ok(());
        }
        if predictor != 2 {
            return Err(WsiError::UnsupportedFormat(format!(
                "unsupported TIFF predictor: {predictor}"
            )));
        }

        let spp = self
            .container
            .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(1) as usize;
        let bps = self
            .container
            .get_u32(ifd_id, tags::BITS_PER_SAMPLE)
            .unwrap_or(8) as usize;
        let width = width as usize;
        let height = height as usize;
        if width == 0 || height == 0 || spp == 0 {
            return Ok(());
        }

        match bps {
            8 => {
                let row_stride = width.checked_mul(spp).ok_or_else(|| {
                    WsiError::UnsupportedFormat("TIFF predictor row stride overflow".into())
                })?;
                if data.len() < row_stride.saturating_mul(height) {
                    return Err(WsiError::TileRead {
                        col: 0,
                        row: 0,
                        level: 0u32,
                        reason: "TIFF predictor payload is shorter than expected".into(),
                    });
                }
                for row in data.chunks_exact_mut(row_stride).take(height) {
                    for idx in spp..row_stride {
                        let prior = row[idx - spp];
                        row[idx] = row[idx].wrapping_add(prior);
                    }
                }
                Ok(())
            }
            16 => {
                let row_samples = width.checked_mul(spp).ok_or_else(|| {
                    WsiError::UnsupportedFormat("TIFF predictor row sample overflow".into())
                })?;
                let row_stride = row_samples.checked_mul(2).ok_or_else(|| {
                    WsiError::UnsupportedFormat("TIFF predictor row stride overflow".into())
                })?;
                if data.len() < row_stride.saturating_mul(height) {
                    return Err(WsiError::TileRead {
                        col: 0,
                        row: 0,
                        level: 0u32,
                        reason: "TIFF predictor payload is shorter than expected".into(),
                    });
                }
                for row in data.chunks_exact_mut(row_stride).take(height) {
                    for sample_idx in spp..row_samples {
                        let byte_idx = sample_idx * 2;
                        let prior_idx = (sample_idx - spp) * 2;
                        let current = match self.container.endian() {
                            Endian::Little => {
                                u16::from_le_bytes([row[byte_idx], row[byte_idx + 1]])
                            }
                            Endian::Big => u16::from_be_bytes([row[byte_idx], row[byte_idx + 1]]),
                        };
                        let prior = match self.container.endian() {
                            Endian::Little => {
                                u16::from_le_bytes([row[prior_idx], row[prior_idx + 1]])
                            }
                            Endian::Big => u16::from_be_bytes([row[prior_idx], row[prior_idx + 1]]),
                        };
                        let value = current.wrapping_add(prior);
                        let bytes = match self.container.endian() {
                            Endian::Little => value.to_le_bytes(),
                            Endian::Big => value.to_be_bytes(),
                        };
                        row[byte_idx..byte_idx + 2].copy_from_slice(&bytes);
                    }
                }
                Ok(())
            }
            _ => Err(WsiError::UnsupportedFormat(format!(
                "unsupported TIFF predictor bits per sample: {bps}"
            ))),
        }
    }

    pub(in super::super) fn decode_compressed_tiff_tile_data(
        &self,
        ifd_id: IfdId,
        compression: Compression,
        input: &[u8],
        width: u32,
        height: u32,
    ) -> Result<CpuTile, WsiError> {
        let expected_bytes = self.expected_uncompressed_tile_bytes(ifd_id, width, height)?;
        let decoded = self.decompress_tiff_payload(
            ifd_id,
            compression,
            input,
            expected_bytes,
            width,
            height,
        )?;
        self.decode_uncompressed_tile(ifd_id, &decoded, width, height)
    }

    pub(in super::super) fn crop_interleaved_top_left(
        tile: CpuTile,
        width: u32,
        height: u32,
    ) -> Result<CpuTile, WsiError> {
        if tile.width == width && tile.height == height {
            return Ok(tile);
        }
        if tile.layout != CpuTileLayout::Interleaved || width > tile.width || height > tile.height {
            return Err(WsiError::DisplayConversion(format!(
                "TIFF edge crop {width}x{height} is incompatible with {}x{} {:?} source",
                tile.width, tile.height, tile.layout
            )));
        }
        let channels = usize::from(tile.channels);
        let source_stride = usize::try_from(tile.width)
            .ok()
            .and_then(|value| value.checked_mul(channels))
            .ok_or_else(|| WsiError::DisplayConversion("TIFF source stride overflow".into()))?;
        let target_stride = usize::try_from(width)
            .ok()
            .and_then(|value| value.checked_mul(channels))
            .ok_or_else(|| WsiError::DisplayConversion("TIFF target stride overflow".into()))?;
        let rows = usize::try_from(height)
            .map_err(|_| WsiError::DisplayConversion("TIFF crop height overflow".into()))?;

        macro_rules! crop_samples {
            ($samples:expr, $constructor:expr) => {{
                let samples = $samples;
                let mut cropped = Vec::with_capacity(target_stride.saturating_mul(rows));
                for row in 0..rows {
                    let start = row.checked_mul(source_stride).ok_or_else(|| {
                        WsiError::DisplayConversion("TIFF crop row offset overflow".into())
                    })?;
                    let end = start.checked_add(target_stride).ok_or_else(|| {
                        WsiError::DisplayConversion("TIFF crop row end overflow".into())
                    })?;
                    cropped.extend_from_slice(&samples[start..end]);
                }
                $constructor(cropped)
            }};
        }

        let data = match &tile.data {
            CpuTileData::U8(samples) => crop_samples!(samples, CpuTileData::u8),
            CpuTileData::U16(samples) => crop_samples!(samples, CpuTileData::u16),
            CpuTileData::F32(samples) => crop_samples!(samples, CpuTileData::f32),
        };
        CpuTile::new(
            width,
            height,
            tile.channels,
            tile.color_space,
            CpuTileLayout::Interleaved,
            data,
        )
    }
}
