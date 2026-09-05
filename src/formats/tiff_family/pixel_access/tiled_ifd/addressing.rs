use super::super::*;

#[derive(Clone, Copy)]
pub(in super::super) struct TiledIfdTileSpan {
    pub(in super::super) offset: u64,
    pub(in super::super) byte_count: u64,
    pub(in super::super) width: u32,
    pub(in super::super) height: u32,
}

impl TiffPixelReader {
    pub(in super::super) fn tiled_ifd_tile_index_and_dimensions(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
    ) -> Result<(usize, u32, u32), WsiError> {
        let col = req.col;
        let row = req.row;

        let level = &self.layout.dataset.scenes[req.scene.get()].series[req.series.get()].levels
            [req.level.get() as usize];

        let tile_idx = match &level.tile_layout {
            TileLayout::Regular {
                tiles_across,
                tiles_down,
                ..
            } => {
                if col < 0 || col >= *tiles_across as i64 || row < 0 || row >= *tiles_down as i64 {
                    return Err(WsiError::TileRead {
                        col,
                        row,
                        level: req.level.get(),
                        reason: format!(
                            "tile ({},{}) out of range ({}x{})",
                            col, row, tiles_across, tiles_down,
                        ),
                    });
                }
                (row as u64 * *tiles_across + col as u64) as usize
            }
            TileLayout::Irregular { tiles, .. } => {
                let entry = tiles.get(&(col, row)).ok_or_else(|| WsiError::TileRead {
                    col,
                    row,
                    level: req.level.get(),
                    reason: format!("no irregular tile at ({},{})", col, row),
                })?;
                if let Some(tile_idx) = entry.tiff_tile_index {
                    tile_idx
                } else {
                    let image_width =
                        self.container
                            .get_u64(ifd_id, tags::IMAGE_WIDTH)
                            .map_err(|err| WsiError::TileRead {
                                col,
                                row,
                                level: req.level.get(),
                                reason: format!("failed to read tiled IFD image width: {err}"),
                            })?;
                    let tile_width =
                        self.container
                            .get_u32(ifd_id, tags::TILE_WIDTH)
                            .map_err(|err| WsiError::TileRead {
                                col,
                                row,
                                level: req.level.get(),
                                reason: format!("failed to read tiled IFD tile width: {err}"),
                            })?;
                    let tiles_across = image_width.div_ceil(tile_width as u64);
                    if col < 0 || row < 0 {
                        return Err(WsiError::TileRead {
                            col,
                            row,
                            level: req.level.get(),
                            reason: "irregular tile row/col out of range for TIFF tile grid".into(),
                        });
                    }
                    (row as u64 * tiles_across + col as u64) as usize
                }
            }
            TileLayout::WholeLevel { .. } => {
                return Err(WsiError::TileRead {
                    col,
                    row,
                    level: req.level.get(),
                    reason: "TiledIfd does not use WholeLevel layout".into(),
                });
            }
        };

        let (level_w, level_h) = level.dimensions;
        let (tw, th) = match &level.tile_layout {
            TileLayout::Regular {
                tile_width,
                tile_height,
                ..
            } => {
                let tw =
                    (*tile_width).min((level_w as u32).saturating_sub(col as u32 * *tile_width));
                let th =
                    (*tile_height).min((level_h as u32).saturating_sub(row as u32 * *tile_height));
                (tw, th)
            }
            TileLayout::Irregular { .. } => {
                let image_width =
                    self.container
                        .get_u64(ifd_id, tags::IMAGE_WIDTH)
                        .map_err(|err| WsiError::TileRead {
                            col,
                            row,
                            level: req.level.get(),
                            reason: format!("failed to read irregular TIFF image width: {err}"),
                        })?;
                let image_height =
                    self.container
                        .get_u64(ifd_id, tags::IMAGE_LENGTH)
                        .map_err(|err| WsiError::TileRead {
                            col,
                            row,
                            level: req.level.get(),
                            reason: format!("failed to read irregular TIFF image height: {err}"),
                        })?;
                let tile_width =
                    self.container
                        .get_u32(ifd_id, tags::TILE_WIDTH)
                        .map_err(|err| WsiError::TileRead {
                            col,
                            row,
                            level: req.level.get(),
                            reason: format!("failed to read irregular TIFF tile width: {err}"),
                        })?;
                let tile_height =
                    self.container
                        .get_u32(ifd_id, tags::TILE_LENGTH)
                        .map_err(|err| WsiError::TileRead {
                            col,
                            row,
                            level: req.level.get(),
                            reason: format!("failed to read irregular TIFF tile height: {err}"),
                        })?;
                let tw = tile_width.min(
                    image_width
                        .saturating_sub(col.max(0) as u64 * tile_width as u64)
                        .try_into()
                        .unwrap_or(u32::MAX),
                );
                let th = tile_height.min(
                    image_height
                        .saturating_sub(row.max(0) as u64 * tile_height as u64)
                        .try_into()
                        .unwrap_or(u32::MAX),
                );
                (tw, th)
            }
            _ => {
                return Err(WsiError::TileRead {
                    col,
                    row,
                    level: req.level.get(),
                    reason: "unexpected tile layout for tiled IFD read".into(),
                });
            }
        };

        Ok((tile_idx, tw, th))
    }

    /// Read a tile from a TiledIfd source (standard TIFF tiled IFDs).
    pub(in super::super) fn read_tiled_ifd_tile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        jpeg_tables: Option<&[u8]>,
        compression: Compression,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let (tile_idx, tw, th) = self.tiled_ifd_tile_index_and_dimensions(req, ifd_id)?;
        let (offsets, byte_counts) = self.tiled_ifd_offsets_and_byte_counts(ifd_id)?;
        self.decode_tiled_ifd_tile_index(
            ifd_id,
            tile_idx,
            jpeg_tables,
            compression,
            tw,
            th,
            offsets,
            byte_counts,
            backend,
        )
        .map_err(|err| match err {
            WsiError::TileRead { .. } => err,
            other => WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: other.to_string(),
            },
        })
    }

    pub(in super::super) fn tiled_ifd_offsets_and_byte_counts(
        &self,
        ifd_id: IfdId,
    ) -> Result<(&[u64], &[u64]), WsiError> {
        let offsets = self
            .container
            .get_u64_array(ifd_id, tags::TILE_OFFSETS)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;
        let byte_counts = self
            .container
            .get_u64_array(ifd_id, tags::TILE_BYTE_COUNTS)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;
        Ok((offsets, byte_counts))
    }

    pub(in super::super) fn tiled_ifd_tile_span(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
    ) -> Result<TiledIfdTileSpan, WsiError> {
        let (tile_idx, width, height) = self.tiled_ifd_tile_index_and_dimensions(req, ifd_id)?;
        let (offsets, byte_counts) = self.tiled_ifd_offsets_and_byte_counts(ifd_id)?;
        if tile_idx >= offsets.len() || tile_idx >= byte_counts.len() {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "tile index {} out of range (offsets={}, byte_counts={})",
                    tile_idx,
                    offsets.len(),
                    byte_counts.len()
                ),
            });
        }
        Ok(TiledIfdTileSpan {
            offset: offsets[tile_idx],
            byte_count: byte_counts[tile_idx],
            width,
            height,
        })
    }

    pub(in super::super) fn read_tiled_ifd_tile_span(
        &self,
        span: TiledIfdTileSpan,
    ) -> Result<Vec<u8>, WsiError> {
        self.container
            .pread(span.offset, span.byte_count)
            .map_err(|err| err.into_wsi_error(self.container.path()))
    }

    #[allow(clippy::too_many_arguments)]
    pub(in super::super) fn decode_tiled_ifd_tile_index(
        &self,
        ifd_id: IfdId,
        tile_idx: usize,
        jpeg_tables: Option<&[u8]>,
        compression: Compression,
        width: u32,
        height: u32,
        offsets: &[u64],
        byte_counts: &[u64],
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        if tile_idx >= offsets.len() || tile_idx >= byte_counts.len() {
            return Err(WsiError::UnsupportedFormat(format!(
                "tile index {} out of range (offsets={}, byte_counts={})",
                tile_idx,
                offsets.len(),
                byte_counts.len(),
            )));
        }

        let offset = offsets[tile_idx];
        let byte_count = byte_counts[tile_idx];
        if byte_count == 0 {
            return if self.layout.dataset.properties.vendor() == Some("generic-tiff") {
                Self::empty_transparent_tile(width, height)
            } else {
                Self::empty_rgb_tile(width, height)
            };
        }

        let tile_data = self
            .container
            .pread(offset, byte_count)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;
        match compression {
            Compression::Jpeg => {
                self.decode_tiled_ifd_jpeg_tile_data(ifd_id, jpeg_tables, &tile_data, width, height)
            }
            Compression::Jp2kRgb => decode_one_jp2k(Jp2kDecodeJob {
                data: Cow::Borrowed(&tile_data),
                expected_width: width,
                expected_height: height,
                rgb_color_space: true,
                backend,
            }),
            Compression::Jp2kYcbcr => decode_one_jp2k(Jp2kDecodeJob {
                data: Cow::Borrowed(&tile_data),
                expected_width: width,
                expected_height: height,
                rgb_color_space: false,
                backend,
            }),
            Compression::None
            | Compression::Lzw
            | Compression::Deflate
            | Compression::Zstd
            | Compression::JpegXr => {
                let physical_width = self
                    .container
                    .get_u32(ifd_id, tags::TILE_WIDTH)
                    .unwrap_or(width);
                let physical_height = self
                    .container
                    .get_u32(ifd_id, tags::TILE_LENGTH)
                    .unwrap_or(height);
                let physical = if compression == Compression::JpegXr {
                    self.decode_jpegxr_tile(ifd_id, &tile_data, physical_width, physical_height)?
                } else if compression == Compression::None {
                    self.decode_uncompressed_tile(
                        ifd_id,
                        &tile_data,
                        physical_width,
                        physical_height,
                    )?
                } else {
                    self.decode_compressed_tiff_tile_data(
                        ifd_id,
                        compression,
                        &tile_data,
                        physical_width,
                        physical_height,
                    )?
                };
                Self::crop_interleaved_top_left(physical, width, height)
            }
            other => Err(WsiError::UnsupportedFormat(format!(
                "unsupported TiledIfd compression: {:?}",
                other,
            ))),
        }
    }

    pub(in super::super) fn read_tiled_ifd_raw_jpeg_tile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        jpeg_tables: Option<&[u8]>,
    ) -> Result<RawCompressedTile, WsiError> {
        let span = self.tiled_ifd_tile_span(req, ifd_id)?;
        if span.byte_count == 0 {
            return Err(WsiError::Unsupported {
                reason: "JPEG passthrough does not support empty TIFF tiles".into(),
            });
        }
        let tile_data = self.read_tiled_ifd_tile_span(span)?;
        let (data, info) = standalone_jpeg_frame_owned(tile_data, jpeg_tables)?;
        Ok(RawCompressedTile::builder(Compression::Jpeg)
            // The encoded JPEG commonly retains a full physical tile at the
            // right and bottom edges. Preserve the TIFF level's logical edge
            // dimensions so device decoders crop exactly like the CPU path.
            .dimensions(span.width, span.height)
            .bits_allocated(info.bits_allocated)
            .samples_per_pixel(info.samples_per_pixel)
            .photometric_interpretation(info.photometric_interpretation)
            .data(data)
            .build()?)
    }

    pub(in super::super) fn read_tiled_ifd_raw_jp2k_tile(
        &self,
        req: &TileRequest,
        ifd_id: IfdId,
        compression: Compression,
    ) -> Result<RawCompressedTile, WsiError> {
        let span = self.tiled_ifd_tile_span(req, ifd_id)?;
        if span.byte_count == 0 {
            return Err(WsiError::Unsupported {
                reason: "J2K passthrough does not support empty TIFF tiles".into(),
            });
        }

        let data = self.read_tiled_ifd_tile_span(span)?;
        let samples_per_pixel = self
            .container
            .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(3);
        if samples_per_pixel == 0 || samples_per_pixel > u32::from(u16::MAX) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "J2K passthrough requires samples per pixel to fit in u16, got {samples_per_pixel}"
                ),
            });
        }
        let bits_allocated = self
            .container
            .get_u32(ifd_id, tags::BITS_PER_SAMPLE)
            .unwrap_or(8);
        if bits_allocated == 0 || bits_allocated > u32::from(u16::MAX) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "J2K passthrough requires bits per sample to fit in u16, got {bits_allocated}"
                ),
            });
        }
        let photometric = self.container.get_u32(ifd_id, tags::PHOTOMETRIC).unwrap_or(
            match (compression, samples_per_pixel) {
                (_, 1) => 1,
                (Compression::Jp2kYcbcr, _) => 6,
                _ => 2,
            },
        );
        let photometric_interpretation = match samples_per_pixel {
            1 => EncodedTilePhotometricInterpretation::Monochrome2,
            3 => match compression {
                Compression::Jp2kRgb => EncodedTilePhotometricInterpretation::Rgb,
                Compression::Jp2kYcbcr => EncodedTilePhotometricInterpretation::YbrFull422,
                _ if photometric == 2 => EncodedTilePhotometricInterpretation::Rgb,
                _ if photometric == 6 => EncodedTilePhotometricInterpretation::YbrFull422,
                _ => {
                    return Err(WsiError::Unsupported {
                        reason: format!(
                            "J2K passthrough does not support photometric interpretation {photometric}"
                        ),
                    });
                }
            },
            other => {
                return Err(WsiError::Unsupported {
                    reason: format!("J2K passthrough supports 1 or 3 samples, got {other}"),
                });
            }
        };

        Ok(RawCompressedTile::builder(compression)
            .dimensions(span.width, span.height)
            .bits_allocated(bits_allocated as u16)
            .samples_per_pixel(samples_per_pixel as u16)
            .photometric_interpretation(photometric_interpretation)
            .data(data)
            .build()?)
    }
}
