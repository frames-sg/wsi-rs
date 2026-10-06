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
        let result = self.decode_tiled_ifd_jpeg_jobs(reqs);
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

    /// Reads each tile on the caller. A pool that fills the machine owns the
    /// decode work; a smaller pool helps callers use the remaining cores.
    fn decode_tiled_ifd_jpeg_jobs(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        let mut tiles: Vec<Option<CpuTile>> = Vec::with_capacity(reqs.len());
        let mut jobs = Vec::new();
        // Jobs from one IFD share its JPEGTables instead of copying them per tile.
        let mut shared_tables: Vec<(IfdId, Option<std::sync::Arc<[u8]>>)> = Vec::new();
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
                    reason: "JPEG tiled batch received a non-JPEG tile source".into(),
                });
            };
            let span = self.tiled_ifd_tile_span(req, *ifd_id)?;
            if span.byte_count == 0 {
                tiles.push(Some(self.empty_tiled_ifd_tile(span.width, span.height)?));
                continue;
            }
            let data = self.read_tiled_ifd_tile_span(span)?;
            let options = self.tiff_jpeg_decode_options_for_data(
                *ifd_id,
                false,
                &data,
                jpeg_tables.as_deref(),
            );
            let tables = match shared_tables.iter().find(|(id, _)| id == ifd_id) {
                Some((_, tables)) => tables.clone(),
                None => {
                    let tables = jpeg_tables.as_deref().map(std::sync::Arc::<[u8]>::from);
                    shared_tables.push((*ifd_id, tables.clone()));
                    tables
                }
            };
            tiles.push(None);
            jobs.push(TiledJpegJob {
                data,
                tables,
                width: span.width,
                height: span.height,
                options,
                position: (req.col, req.row, req.level.get()),
            });
        }
        let decoded = if jobs.is_empty() {
            Vec::new()
        } else {
            let runtime = crate::core::decode_runtime::DecodeRuntime::default_arc();
            let decode = || crate::core::batch::share_cpu_work(jobs, TiledJpegJob::decode);
            if runtime.cpu_worker_count() >= crate::core::batch::cpu_core_count() {
                runtime.install_jp2k_cpu(decode)
            } else {
                decode()
            }
        };
        let mut decoded = decoded.into_iter();
        tiles
            .into_iter()
            .map(|tile| match tile {
                Some(tile) => Ok(tile),
                None => decoded.next().expect("one result per decode job"),
            })
            .collect()
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    pub(in super::super) fn collect_tiled_ifd_jp2k_jobs(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<Jp2kDecodeJob<'static>>, WsiError> {
        let mut jobs = Vec::with_capacity(reqs.len());
        for req in reqs {
            if let Some(control) = control {
                control.check_cancelled()?;
            }
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
        let jobs = self.collect_tiled_ifd_jp2k_jobs(reqs, BackendRequest::Metal, None)?;
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
        let jobs = self.collect_tiled_ifd_jp2k_jobs(reqs, BackendRequest::Cuda, None)?;
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

/// One tiled-IFD JPEG tile with its encoded bytes already read.
struct TiledJpegJob {
    data: Vec<u8>,
    tables: Option<std::sync::Arc<[u8]>>,
    width: u32,
    height: u32,
    options: TiffJpegDecodeOptions,
    position: (i64, i64, u32),
}

impl TiledJpegJob {
    fn decode(&self) -> Result<CpuTile, WsiError> {
        let (col, row, level) = self.position;
        decode_one_jpeg(JpegDecodeJob {
            data: Cow::Borrowed(&self.data),
            tables: self.tables.as_deref().map(Cow::Borrowed),
            expected_width: self.width,
            expected_height: self.height,
            color_transform: self.options.color_transform,
            force_dimensions: self.options.force_dimensions,
            requested_size: None,
        })
        .map_err(|err| match err {
            WsiError::TileRead { .. } => err,
            other => WsiError::TileRead {
                col,
                row,
                level,
                reason: other.to_string(),
            },
        })
    }
}
