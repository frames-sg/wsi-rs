use super::*;

/// The fields of `TileSource::StrippedLevel`, borrowed for one tile read.
pub(super) struct StrippedLevelSource<'a> {
    pub(super) ifd_id: IfdId,
    pub(super) compression: Compression,
    pub(super) jpeg_tables: Option<&'a [u8]>,
    pub(super) rows_per_strip: u32,
    pub(super) strip_offsets: &'a [u64],
    pub(super) strip_byte_counts: &'a [u64],
}

impl StrippedLevelSource<'_> {
    /// `(offset, byte_count)` of strip `index`.
    fn strip(&self, index: usize) -> Result<(u64, u64), WsiError> {
        match (
            self.strip_offsets.get(index),
            self.strip_byte_counts.get(index),
        ) {
            (Some(&offset), Some(&byte_count)) => Ok((offset, byte_count)),
            _ => Err(WsiError::UnsupportedFormat(format!(
                "generic stripped TIFF strip {index} is missing from the strip table \
                 ({} offsets, {} byte counts)",
                self.strip_offsets.len(),
                self.strip_byte_counts.len()
            ))),
        }
    }
}

/// The pixels one tile covers in an `image_width` x `image_height` level.
#[derive(Clone, Copy)]
struct StripWindow {
    image_width: u32,
    image_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl TiffPixelReader {
    pub(super) fn read_stripped_level_tile(
        &self,
        req: &TileRequest,
        source: &StrippedLevelSource<'_>,
    ) -> Result<CpuTile, WsiError> {
        if source.rows_per_strip == 0 {
            return Err(WsiError::tile_read(
                req,
                "generic stripped TIFF level has zero rows per strip",
            ));
        }
        let window = self.stripped_level_window(req)?;
        if source.compression == Compression::None {
            self.read_uncompressed_strip_window(req, source, window)
        } else {
            self.compose_decoded_strip_window(req, source, window)
        }
    }

    fn stripped_level_window(&self, req: &TileRequest) -> Result<StripWindow, WsiError> {
        let (_, _, level) = self
            .layout
            .dataset
            .level(req.scene, req.series, req.level)?;
        let TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } = level.tile_layout
        else {
            return Err(WsiError::tile_read(
                req,
                "generic stripped TIFF level expects a regular tile layout",
            ));
        };
        let (col, row) = validate_tile_coords(req.col, req.row, req.level.get())?;
        if u64::from(col) >= tiles_across || u64::from(row) >= tiles_down {
            return Err(WsiError::tile_read(
                req,
                format!("tile is outside the {tiles_across}x{tiles_down} grid"),
            ));
        }

        let image_width = u32::try_from(level.dimensions.0)
            .map_err(|_| WsiError::tile_read(req, "generic stripped TIFF width exceeds u32"))?;
        let image_height = u32::try_from(level.dimensions.1)
            .map_err(|_| WsiError::tile_read(req, "generic stripped TIFF height exceeds u32"))?;
        let x = col * tile_width;
        let y = row * tile_height;
        Ok(StripWindow {
            image_width,
            image_height,
            x,
            y,
            width: tile_width.min(image_width - x),
            height: tile_height.min(image_height - y),
        })
    }

    /// Reads an uncompressed window straight from the file, one row span per
    /// read, so a tile costs only the bytes it covers.
    fn read_uncompressed_strip_window(
        &self,
        req: &TileRequest,
        source: &StrippedLevelSource<'_>,
        window: StripWindow,
    ) -> Result<CpuTile, WsiError> {
        let samples = self
            .container
            .get_u32(source.ifd_id, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(1);
        let planar = samples == 3
            && self
                .container
                .get_u32(source.ifd_id, tags::PLANAR_CONFIGURATION)
                .unwrap_or(1)
                == 2;
        if !planar {
            let data = self.read_uncompressed_plane_window(req, source, window, 0, samples)?;
            return self.decode_uncompressed_tile(
                source.ifd_id,
                &data,
                window.width,
                window.height,
            );
        }

        // The support check admits separate planes only for 8-bit RGB.
        let planes = [
            self.read_uncompressed_plane_window(req, source, window, 0, 1)?,
            self.read_uncompressed_plane_window(req, source, window, 1, 1)?,
            self.read_uncompressed_plane_window(req, source, window, 2, 1)?,
        ];
        let interleaved = (0..planes[0].len())
            .flat_map(|pixel| planes.iter().map(move |plane| plane[pixel]))
            .collect();
        CpuTile::from_u8_interleaved(window.width, window.height, 3, ColorSpace::Rgb, interleaved)
    }

    /// Raw bytes of `window` in sample plane `plane`, where each stored pixel
    /// holds `stored_samples` 8-bit samples.
    fn read_uncompressed_plane_window(
        &self,
        req: &TileRequest,
        source: &StrippedLevelSource<'_>,
        window: StripWindow,
        plane: usize,
        stored_samples: u32,
    ) -> Result<Vec<u8>, WsiError> {
        let overflow = || WsiError::tile_read(req, "generic stripped TIFF row offset overflow");
        let row_bytes = u64::from(window.image_width) * u64::from(stored_samples);
        let span_start = u64::from(window.x) * u64::from(stored_samples);
        let span_len = u64::from(window.width) * u64::from(stored_samples);
        let strips_per_plane = window.image_height.div_ceil(source.rows_per_strip) as usize;
        let window_len = checked_product_to_usize(
            &[span_len, u64::from(window.height)],
            MAX_DECODED_IMAGE_BYTES,
            "generic stripped TIFF tile",
        )
        .map_err(|reason| WsiError::tile_read(req, reason))?;

        let mut data = Vec::with_capacity(window_len);
        for y in window.y..window.y + window.height {
            let index = plane * strips_per_plane + (y / source.rows_per_strip) as usize;
            let (offset, byte_count) = source.strip(index)?;
            let row_start = u64::from(y % source.rows_per_strip)
                .checked_mul(row_bytes)
                .and_then(|start| start.checked_add(span_start))
                .ok_or_else(overflow)?;
            let row_end = row_start.checked_add(span_len).ok_or_else(overflow)?;
            if row_end > byte_count {
                return Err(WsiError::tile_read(
                    req,
                    format!(
                        "uncompressed strip {index} holds {byte_count} bytes but row {y} \
                         ends at byte {row_end}"
                    ),
                ));
            }
            let read_at = offset.checked_add(row_start).ok_or_else(overflow)?;
            let bytes = self
                .container
                .pread(read_at, span_len)
                .map_err(|err| err.into_wsi_error(self.container.path()))?;
            data.extend_from_slice(&bytes);
        }
        Ok(data)
    }

    /// Copies `window` out of the decoded strips its rows fall in.
    fn compose_decoded_strip_window(
        &self,
        req: &TileRequest,
        source: &StrippedLevelSource<'_>,
        window: StripWindow,
    ) -> Result<CpuTile, WsiError> {
        let rows_per_strip = source.rows_per_strip;
        let window_bottom = window.y + window.height;
        let mut composed = Vec::new();
        let mut pixel_format: Option<(u16, ColorSpace)> = None;

        for strip_row in window.y / rows_per_strip..=(window_bottom - 1) / rows_per_strip {
            let strip_y = strip_row * rows_per_strip;
            let strip_height = rows_per_strip.min(window.image_height - strip_y);
            let strip = self.decoded_level_strip(
                req,
                source,
                strip_row as usize,
                window.image_width,
                strip_height,
            )?;
            let channels = usize::from(strip.channels);
            let stride = window.image_width as usize * channels;
            let samples = strip
                .data
                .as_u8()
                .filter(|samples| {
                    strip.layout == CpuTileLayout::Interleaved
                        && strip.width == window.image_width
                        && strip.height == strip_height
                        && stride.checked_mul(strip_height as usize) == Some(samples.len())
                })
                .ok_or_else(|| {
                    WsiError::tile_read(
                        req,
                        format!(
                            "strip {strip_row} decoded as {}x{} {:?} {:?}, expected {}x{} \
                             interleaved 8-bit samples",
                            strip.width,
                            strip.height,
                            strip.layout,
                            strip.data.sample_type(),
                            window.image_width,
                            strip_height
                        ),
                    )
                })?;

            let strip_format = (strip.channels, strip.color_space.clone());
            match &pixel_format {
                None => {
                    composed.reserve(window.width as usize * window.height as usize * channels);
                    pixel_format = Some(strip_format);
                }
                Some(first) if *first != strip_format => {
                    return Err(WsiError::tile_read(
                        req,
                        format!(
                            "strip {strip_row} decoded as {strip_format:?}, other strips as {first:?}"
                        ),
                    ));
                }
                Some(_) => {}
            }

            let span_start = window.x as usize * channels;
            let span_len = window.width as usize * channels;
            let first_row = window.y.max(strip_y) - strip_y;
            let end_row = window_bottom.min(strip_y + strip_height) - strip_y;
            for row in first_row..end_row {
                let start = row as usize * stride + span_start;
                composed.extend_from_slice(&samples[start..start + span_len]);
            }
        }

        let (channels, color_space) =
            pixel_format.ok_or_else(|| WsiError::tile_read(req, "tile covers no strips"))?;
        CpuTile::new(
            window.width,
            window.height,
            channels,
            color_space,
            CpuTileLayout::Interleaved,
            CpuTileData::u8(composed),
        )
    }

    /// Strip `index`, `width` x `height` pixels, decoded once and shared
    /// through the full-decode cache by every tile that covers it.
    fn decoded_level_strip(
        &self,
        req: &TileRequest,
        source: &StrippedLevelSource<'_>,
        index: usize,
        width: u32,
        height: u32,
    ) -> Result<Arc<CpuTile>, WsiError> {
        let decode_error = |reason: String| WsiError::tile_read(req, reason);
        self.full_decode_cache.get_or_try_insert_with_error(
            FullDecodeKey::Strip {
                ifd_id: source.ifd_id,
                index,
            },
            || {
                self.decode_level_strip(source, index, width, height)
                    .map(Arc::new)
                    .map_err(|err| decode_error(format!("strip {index}: {err}")))
            },
            decode_error,
        )
    }

    fn decode_level_strip(
        &self,
        source: &StrippedLevelSource<'_>,
        index: usize,
        width: u32,
        height: u32,
    ) -> Result<CpuTile, WsiError> {
        let (offset, byte_count) = source.strip(index)?;
        if byte_count > MAX_COMPRESSED_INPUT_BYTES {
            return Err(WsiError::UnsupportedFormat(format!(
                "compressed strip holds {byte_count} bytes, over the \
                 {MAX_COMPRESSED_INPUT_BYTES}-byte limit"
            )));
        }
        let data = self
            .container
            .pread(offset, byte_count)
            .map_err(|err| err.into_wsi_error(self.container.path()))?;
        match source.compression {
            Compression::Jpeg => {
                // Some writers size the last strip's frame to RowsPerStrip;
                // forcing the dimensions decodes only the rows it holds.
                let options = self.tiff_jpeg_decode_options_for_data(
                    source.ifd_id,
                    true,
                    &data,
                    source.jpeg_tables,
                );
                decode_one_jpeg(JpegDecodeJob {
                    data: Cow::Borrowed(&data),
                    tables: source.jpeg_tables.map(Cow::Borrowed),
                    expected_width: width,
                    expected_height: height,
                    color_transform: options.color_transform,
                    force_dimensions: options.force_dimensions,
                    requested_size: None,
                })
            }
            Compression::Lzw | Compression::Deflate | Compression::Zstd => self
                .decode_compressed_tiff_tile_data(
                    source.ifd_id,
                    source.compression,
                    &data,
                    width,
                    height,
                ),
            other => Err(WsiError::UnsupportedFormat(format!(
                "generic stripped TIFF levels cannot decode {other:?} strips"
            ))),
        }
    }
}
