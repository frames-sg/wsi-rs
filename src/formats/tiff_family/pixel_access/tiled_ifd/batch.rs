use super::super::*;

impl TiffPixelReader {
    pub(in super::super) fn tiled_ifd_batch_compression(
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

    pub(in super::super) fn decode_tiled_ifd_mixed_batch(
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

    pub(in super::super) fn decode_tiled_ifd_jpeg_batch(
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
    fn collect_tiled_ifd_jp2k_jobs(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
    ) -> Result<Vec<Jp2kDecodeJob<'static>>, WsiError> {
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
            if !matches!(
                actual_compression,
                Compression::Jp2kRgb | Compression::Jp2kYcbcr
            ) {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "strict TIFF device reads support JP2K/HTJ2K tiles only".into(),
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
                rgb_color_space: matches!(actual_compression, Compression::Jp2kRgb),
                backend,
            });
        }
        Ok(jobs)
    }

    #[cfg(feature = "metal")]
    pub(in super::super) fn decode_tiled_ifd_jp2k_metal(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        let jobs = self.collect_tiled_ifd_jp2k_jobs(reqs, BackendRequest::Metal)?;
        crate::decode::jp2k::decode_batch_jp2k_metal(&jobs, sessions)
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

    #[cfg(feature = "cuda")]
    pub(in super::super) fn decode_tiled_ifd_jp2k_cuda(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        let jobs = self.collect_tiled_ifd_jp2k_jobs(reqs, BackendRequest::Cuda)?;
        crate::decode::jp2k::decode_batch_jp2k_cuda(&jobs, sessions)
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
}
