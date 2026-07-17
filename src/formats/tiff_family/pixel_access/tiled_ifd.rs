use super::*;

#[derive(Clone, Copy)]
pub(super) struct TiledIfdTileSpan {
    pub(super) offset: u64,
    pub(super) byte_count: u64,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl TiffPixelReader {
    pub(super) fn tiff_jpeg_decode_options_for_data(
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

    pub(super) fn tiff_jpeg_decode_options_with_hint(
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

    pub(super) fn tiled_ifd_tile_index_and_dimensions(
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
    pub(super) fn read_tiled_ifd_tile(
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

    pub(super) fn tiled_ifd_offsets_and_byte_counts(
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

    pub(super) fn tiled_ifd_tile_span(
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

    pub(super) fn read_tiled_ifd_tile_span(
        &self,
        span: TiledIfdTileSpan,
    ) -> Result<Vec<u8>, WsiError> {
        self.container
            .pread(span.offset, span.byte_count)
            .map_err(|err| err.into_wsi_error(self.container.path()))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn decode_tiled_ifd_tile_index(
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
            return Self::empty_rgb_tile(width, height);
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
            Compression::None => {
                // Uncompressed: interpret raw bytes using TIFF metadata
                self.decode_uncompressed_tile(ifd_id, &tile_data, width, height)
            }
            Compression::Lzw | Compression::Deflate | Compression::Zstd => self
                .decode_compressed_tiff_tile_data(ifd_id, compression, &tile_data, width, height),
            other => Err(WsiError::UnsupportedFormat(format!(
                "unsupported TiledIfd compression: {:?}",
                other,
            ))),
        }
    }

    pub(super) fn decode_tiled_ifd_jpeg_tile_data(
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

    pub(super) fn read_tiled_ifd_raw_jpeg_tile(
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

    pub(super) fn read_tiled_ifd_raw_jp2k_tile(
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

    pub(super) fn empty_rgb_tile(width: u32, height: u32) -> Result<CpuTile, WsiError> {
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

    pub(super) fn tiled_ifd_batch_compression(
        &self,
        reqs: &[TileRequest],
    ) -> Result<Option<Compression>, WsiError> {
        let mut batch_compression = None;
        for req in reqs {
            let TileSource::TiledIfd { compression, .. } = self.tile_source_for(req)? else {
                return Ok(None);
            };
            if !matches!(
                compression,
                Compression::Jpeg | Compression::Jp2kRgb | Compression::Jp2kYcbcr
            ) {
                return Ok(None);
            }
            match batch_compression {
                Some(existing) if existing != *compression => return Ok(None),
                Some(_) => {}
                None => batch_compression = Some(*compression),
            }
        }
        Ok(batch_compression)
    }

    pub(super) fn decode_tiled_ifd_mixed_batch(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
    ) -> Result<Option<Vec<CpuTile>>, WsiError> {
        let mut jobs = Vec::with_capacity(reqs.len());
        for req in reqs {
            let source = self.tile_source_for(req)?;
            let TileSource::TiledIfd {
                ifd_id,
                jpeg_tables,
                compression,
            } = source
            else {
                return Ok(None);
            };
            if !matches!(
                compression,
                Compression::Jpeg | Compression::Jp2kRgb | Compression::Jp2kYcbcr
            ) {
                return Ok(None);
            }

            let span = self.tiled_ifd_tile_span(req, *ifd_id)?;
            if span.byte_count == 0 {
                return Ok(None);
            }
            let data = self.read_tiled_ifd_tile_span(span)?;

            let job = match compression {
                Compression::Jpeg => {
                    let options = self.tiff_jpeg_decode_options_for_data(
                        *ifd_id,
                        false,
                        &data,
                        jpeg_tables.as_deref(),
                    );
                    CodecBatchJob::Jpeg(JpegDecodeJob {
                        data: Cow::Owned(data),
                        tables: jpeg_tables.as_deref().map(Cow::Borrowed),
                        expected_width: span.width,
                        expected_height: span.height,
                        color_transform: options.color_transform,
                        force_dimensions: options.force_dimensions,
                        requested_size: None,
                    })
                }
                Compression::Jp2kRgb | Compression::Jp2kYcbcr => {
                    CodecBatchJob::Jp2k(Jp2kDecodeJob {
                        data: Cow::Owned(data),
                        expected_width: span.width,
                        expected_height: span.height,
                        rgb_color_space: matches!(compression, Compression::Jp2kRgb),
                        backend,
                    })
                }
                _ => unreachable!("filtered above"),
            };
            jobs.push(job);
        }

        decode_mixed_batch(jobs)?
            .into_iter()
            .zip(reqs.iter())
            .map(|(result, req)| {
                result.map_err(|err| WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: err.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    pub(super) fn decode_tiled_ifd_jpeg_batch(
        &self,
        reqs: &[TileRequest],
        _backend: BackendRequest,
    ) -> Result<Vec<CpuTile>, WsiError> {
        let started = tracing::enabled!(tracing::Level::DEBUG).then(std::time::Instant::now);
        let result: Result<Vec<CpuTile>, WsiError> = reqs
            .par_iter()
            .map(|req| {
                let source = self.tile_source_for(req)?;
                let TileSource::TiledIfd {
                    ifd_id,
                    jpeg_tables,
                    compression: Compression::Jpeg,
                } = source
                else {
                    return Err(WsiError::TileRead {
                        col: req.col,
                        row: req.row,
                        level: req.level.get(),
                        reason: "JPEG tiled batch received a non-JPEG tile source".into(),
                    });
                };

                let span = self.tiled_ifd_tile_span(req, *ifd_id)?;
                if span.byte_count == 0 {
                    return Self::empty_rgb_tile(span.width, span.height);
                }

                let tile_data = self.read_tiled_ifd_tile_span(span)?;
                let options = self.tiff_jpeg_decode_options_for_data(
                    *ifd_id,
                    false,
                    &tile_data,
                    jpeg_tables.as_deref(),
                );
                decode_one_jpeg(JpegDecodeJob {
                    data: Cow::Borrowed(&tile_data),
                    tables: jpeg_tables.as_deref().map(Cow::Borrowed),
                    expected_width: span.width,
                    expected_height: span.height,
                    color_transform: options.color_transform,
                    force_dimensions: options.force_dimensions,
                    requested_size: None,
                })
                .map_err(|err| match err {
                    WsiError::TileRead { .. } => err,
                    other => WsiError::TileRead {
                        col: req.col,
                        row: req.row,
                        level: req.level.get(),
                        reason: other.to_string(),
                    },
                })
            })
            .collect();
        if let Some(started) = started.as_ref() {
            match &result {
                Ok(tiles) => {
                    tracing::debug!(
                        requested_tiles = reqs.len(),
                        decoded_tiles = tiles.len(),
                        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                        "wsi tiff tiled-ifd jpeg batch decoded"
                    );
                }
                Err(err) => {
                    tracing::debug!(
                        requested_tiles = reqs.len(),
                        error = %err,
                        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                        "wsi tiff tiled-ifd jpeg batch failed"
                    );
                }
            }
        }
        result
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    pub(super) fn decode_tiled_ifd_jpeg_pixels(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
        require_device: bool,
        metal_sessions: MetalBackendSessionsRef<'_>,
        cuda_sessions: CudaBackendSessionsRef<'_>,
    ) -> Result<Vec<TilePixels>, WsiError> {
        let jobs = self.collect_tiled_ifd_jpeg_jobs(reqs)?;
        decode_batch_jpeg_pixels(
            &jobs,
            backend,
            require_device,
            metal_sessions,
            cuda_sessions,
        )
        .into_iter()
        .zip(reqs.iter())
        .map(|(result, req)| {
            result.map_err(|err| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: err.to_string(),
            })
        })
        .collect()
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    pub(super) fn collect_tiled_ifd_jpeg_jobs<'a>(
        &'a self,
        reqs: &[TileRequest],
    ) -> Result<Vec<JpegDecodeJob<'a>>, WsiError> {
        let mut jobs = Vec::with_capacity(reqs.len());
        for req in reqs {
            let source = self.tile_source_for(req)?;
            let TileSource::TiledIfd {
                ifd_id,
                jpeg_tables,
                compression: Compression::Jpeg,
            } = source
            else {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "JPEG tiled device batch received a non-JPEG tile source".into(),
                });
            };

            let span = self.tiled_ifd_tile_span(req, *ifd_id)?;
            if span.byte_count == 0 {
                return Err(WsiError::Unsupported {
                    reason: "device backend not available for empty jpeg tile".into(),
                });
            }

            let tile_data = self.read_tiled_ifd_tile_span(span)?;
            let options = self.tiff_jpeg_decode_options_for_data(
                *ifd_id,
                false,
                &tile_data,
                jpeg_tables.as_deref(),
            );
            jobs.push(JpegDecodeJob {
                data: Cow::Owned(tile_data),
                tables: jpeg_tables.as_deref().map(Cow::Borrowed),
                expected_width: span.width,
                expected_height: span.height,
                color_transform: options.color_transform,
                force_dimensions: options.force_dimensions,
                requested_size: None,
            });
        }
        Ok(jobs)
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    pub(super) fn decode_tiled_ifd_jp2k_pixels(
        &self,
        reqs: &[TileRequest],
        compression: Compression,
        backend: BackendRequest,
        require_device: bool,
        metal_sessions: MetalBackendSessionsRef<'_>,
        cuda_sessions: CudaBackendSessionsRef<'_>,
    ) -> Result<Vec<TilePixels>, WsiError> {
        let mut jobs = Vec::with_capacity(reqs.len());
        for req in reqs {
            let source = self.tile_source_for(req)?;
            let TileSource::TiledIfd {
                ifd_id,
                compression: actual_compression,
                ..
            } = source
            else {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "JP2K tiled device batch received a non-tiled tile source".into(),
                });
            };
            if *actual_compression != compression {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "JP2K tiled device batch received mixed compression".into(),
                });
            }

            let span = self.tiled_ifd_tile_span(req, *ifd_id)?;
            if span.byte_count == 0 {
                return Err(WsiError::Unsupported {
                    reason: "device backend not available for empty jp2k tile".into(),
                });
            }
            let data = self.read_tiled_ifd_tile_span(span)?;
            jobs.push(Jp2kDecodeJob {
                data: Cow::Owned(data),
                expected_width: span.width,
                expected_height: span.height,
                rgb_color_space: matches!(compression, Compression::Jp2kRgb),
                backend,
            });
        }

        decode_batch_jp2k_pixels(&jobs, require_device, metal_sessions, cuda_sessions)
            .into_iter()
            .zip(reqs.iter())
            .map(|(result, req)| {
                result.map_err(|err| WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: err.to_string(),
                })
            })
            .collect()
    }

    /// Decode an uncompressed TIFF tile using IFD metadata.
    pub(super) fn decode_uncompressed_tile(
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

    pub(super) fn expected_uncompressed_tile_bytes(
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
        (width as usize)
            .checked_mul(height as usize)
            .and_then(|value| value.checked_mul(spp as usize))
            .and_then(|value| value.checked_mul((bps / 8) as usize))
            .ok_or_else(|| WsiError::UnsupportedFormat("compressed TIFF tile size overflow".into()))
    }

    pub(super) fn decompress_tiff_payload(
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
            Compression::Lzw => {
                let mut pool = LzwPool::new();
                LzwCodec::decompress_into(&mut pool, input, &mut out)
            }
            Compression::Deflate => {
                let mut pool = DeflatePool::new();
                DeflateCodec::decompress_into(&mut pool, input, &mut out)
            }
            Compression::Zstd => {
                let mut pool = ZstdPool::new();
                ZstdCodec::decompress_into(&mut pool, input, &mut out)
            }
            other => {
                return Err(WsiError::UnsupportedFormat(format!(
                    "compression {:?} is not a tilecodec payload",
                    other
                )));
            }
        }
        .map_err(|err| WsiError::Codec {
            codec: match compression {
                Compression::Lzw => "tiff-lzw",
                Compression::Deflate => "tiff-deflate",
                Compression::Zstd => "tiff-zstd",
                _ => "tiff-tilecodec",
            },
            source: Box::new(err),
        })?;
        out.truncate(written);
        self.apply_tiff_predictor(ifd_id, width, height, &mut out)?;
        Ok(out)
    }

    pub(super) fn apply_tiff_predictor(
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

    pub(super) fn decode_compressed_tiff_tile_data(
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
}
