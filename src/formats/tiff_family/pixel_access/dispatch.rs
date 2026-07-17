use super::*;

impl TiffPixelReader {
    /// Look up the TileSource for a given tile request.
    pub(super) fn tile_source_for(&self, req: &TileRequest) -> Result<&TileSource, WsiError> {
        let key = TileSourceKey {
            scene: req.scene.get(),
            series: req.series.get(),
            level: req.level.get(),
            z: req.plane.get().z,
            c: req.plane.get().c,
            t: req.plane.get().t,
        };
        self.layout
            .tile_sources
            .get(&key)
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "no tile source for scene={}, series={}, level={}, z={}, c={}, t={}",
                    req.scene.get(),
                    req.series.get(),
                    req.level.get(),
                    req.plane.get().z,
                    req.plane.get().c,
                    req.plane.get().t,
                ),
            })
    }

    pub(super) fn read_tiles_cpu_with_backend(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
    ) -> Result<Vec<CpuTile>, WsiError> {
        if reqs.is_empty() {
            return Ok(Vec::new());
        }

        let first_source = self.tile_source_for(&reqs[0])?;
        if matches!(
            first_source,
            TileSource::TiledIfd {
                compression: Compression::Jpeg | Compression::Jp2kRgb | Compression::Jp2kYcbcr,
                ..
            }
        ) {
            if self.tiled_ifd_batch_compression(reqs)? == Some(Compression::Jpeg) {
                return self.decode_tiled_ifd_jpeg_batch(reqs, backend);
            }
            if let Some(tiles) = self.decode_tiled_ifd_mixed_batch(reqs, backend)? {
                return Ok(tiles);
            }
        }

        let mut decode_reqs = Vec::with_capacity(reqs.len());
        for req in reqs {
            let source = self.tile_source_for(req)?;
            let TileSource::TiledIfd {
                ifd_id,
                compression,
                ..
            } = source
            else {
                return reqs
                    .iter()
                    .map(|req| self.read_tile_cpu_with_backend_request(req, backend))
                    .collect();
            };
            let colorspace = match compression {
                Compression::Jp2kRgb => Jp2kColorSpace::Rgb,
                Compression::Jp2kYcbcr => Jp2kColorSpace::YCbCr,
                _ => {
                    return reqs
                        .iter()
                        .map(|req| self.read_tile_cpu_with_backend_request(req, backend))
                        .collect();
                }
            };

            let span = self.tiled_ifd_tile_span(req, *ifd_id)?;
            if span.byte_count == 0 {
                return reqs
                    .iter()
                    .map(|req| self.read_tile_cpu_with_backend_request(req, backend))
                    .collect();
            }
            let data = self.read_tiled_ifd_tile_span(span)?;
            decode_reqs.push(Jp2kDecodeJob {
                data: Cow::Owned(data),
                expected_width: span.width,
                expected_height: span.height,
                rgb_color_space: matches!(colorspace, Jp2kColorSpace::Rgb),
                backend,
            });
        }

        decode_batch_jp2k(&decode_reqs)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| {
                let first = &reqs[0];
                WsiError::TileRead {
                    col: first.col,
                    row: first.row,
                    level: first.level.get(),
                    reason: err.to_string(),
                }
            })
    }

    pub(super) fn read_tile_cpu_with_backend_request(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        match self.tile_source_for(req)? {
            TileSource::TiledIfd {
                ifd_id,
                jpeg_tables,
                compression,
            } => self.read_tiled_ifd_tile(
                req,
                *ifd_id,
                jpeg_tables.as_deref(),
                *compression,
                backend,
            ),
            _ => self.read_tile_cpu(req),
        }
    }
}

impl SlideReader for TiffPixelReader {
    fn dataset(&self) -> &Dataset {
        &self.layout.dataset
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        match self.tile_source_for(req) {
            Ok(TileSource::NdpiJpeg { .. } | TileSource::NdpiFullDecode { .. }) => {
                TileCodecKind::Jpeg
            }
            Ok(TileSource::TiledIfd { compression, .. }) => {
                TileCodecKind::from_compression(*compression)
            }
            Ok(TileSource::SyntheticDownsample { base_level, .. }) => {
                let mut base_req = req.clone();
                base_req.level = (*base_level).into();
                self.tile_codec_kind(&base_req)
            }
            Ok(_) | Err(_) => TileCodecKind::Other,
        }
    }

    fn level_source_kind(
        &self,
        scene: SceneId,
        series: SeriesId,
        level: LevelIdx,
    ) -> Result<LevelSourceKind, WsiError> {
        let scene_ref =
            self.layout
                .dataset
                .scenes
                .get(scene.get())
                .ok_or(WsiError::SceneOutOfRange {
                    index: scene.get(),
                    count: self.layout.dataset.scenes.len(),
                })?;
        let series_ref = scene_ref
            .series
            .get(series.get())
            .ok_or(WsiError::SeriesOutOfRange {
                index: series.get(),
                count: scene_ref.series.len(),
            })?;
        if level.get() as usize >= series_ref.levels.len() {
            return Err(WsiError::LevelOutOfRange {
                level: level.get(),
                count: series_ref.levels.len() as u32,
            });
        }

        let synthetic = self.layout.tile_sources.iter().any(|(key, source)| {
            key.scene == scene.get()
                && key.series == series.get()
                && key.level == level.get()
                && matches!(source, TileSource::SyntheticDownsample { .. })
        });
        if synthetic {
            Ok(LevelSourceKind::SyntheticDownsample)
        } else {
            Ok(LevelSourceKind::Physical)
        }
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        match self.tile_source_for(req)? {
            TileSource::TiledIfd {
                ifd_id,
                jpeg_tables,
                compression: Compression::Jpeg,
            } => self.read_tiled_ifd_raw_jpeg_tile(req, *ifd_id, jpeg_tables.as_deref()),
            TileSource::TiledIfd {
                ifd_id,
                compression: compression @ (Compression::Jp2kRgb | Compression::Jp2kYcbcr),
                ..
            } => self.read_tiled_ifd_raw_jp2k_tile(req, *ifd_id, *compression),
            TileSource::TiledIfd { compression, .. } => Err(WsiError::Unsupported {
                reason: format!(
                    "compressed passthrough requires TIFF JPEG or J2K compression, got {:?}",
                    compression
                ),
            }),
            TileSource::NdpiJpeg {
                ifd_id,
                jpeg_header,
                mcu_starts_tag,
                tiles_across,
                tiles_down,
                restart_interval,
                strip_offset,
                strip_byte_count,
                ..
            } => self.read_ndpi_raw_jpeg_tile(
                req,
                *ifd_id,
                jpeg_header,
                *mcu_starts_tag,
                *tiles_across,
                *tiles_down,
                *restart_interval,
                *strip_offset,
                *strip_byte_count,
            ),
            TileSource::NdpiFullDecode { .. } => Err(WsiError::Unsupported {
                reason: "NDPI JPEG passthrough is not available for whole-level full-decode JPEG sources".into(),
            }),
            TileSource::SyntheticDownsample { .. } => Err(WsiError::Unsupported {
                reason: "JPEG passthrough is not available for synthetic downsample levels".into(),
            }),
            TileSource::Stripped { .. }
            | TileSource::StrippedLevel { .. }
            | TileSource::ExternalJpeg { .. } => Err(WsiError::Unsupported {
                reason: "JPEG passthrough is only available for tiled image levels".into(),
            }),
        }
    }

    fn read_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        let tile_req = TileRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: req.level.get().into(),
            plane: req.plane,
            col: req.col,
            row: req.row,
        };
        match self.tile_source_for(&tile_req)? {
            TileSource::NdpiJpeg {
                ifd_id,
                jpeg_header,
                mcu_starts_tag,
                tiles_across,
                tiles_down,
                restart_interval,
                strip_offset,
                strip_byte_count,
            } => self.read_ndpi_raw_compressed_display_tile(
                req,
                *ifd_id,
                jpeg_header,
                *mcu_starts_tag,
                *tiles_across,
                *tiles_down,
                *restart_interval,
                *strip_offset,
                *strip_byte_count,
            ),
            TileSource::NdpiFullDecode { .. } => Err(WsiError::Unsupported {
                reason: "NDPI JPEG retile is not available for whole-level full-decode JPEG sources"
                    .into(),
            }),
            TileSource::SyntheticDownsample { .. } => Err(WsiError::Unsupported {
                reason: "raw compressed display tile access is not available for synthetic downsample levels"
                    .into(),
            }),
            _ => Err(WsiError::Unsupported {
                reason: "raw compressed display tile access is only implemented for NDPI JPEG restart levels"
                    .into(),
            }),
        }
    }

    fn use_display_tile_cache(&self, req: &TileViewRequest) -> bool {
        let tile_req = TileRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: req.level.get().into(),
            plane: req.plane,
            col: req.col,
            row: req.row,
        };
        match self.tile_source_for(&tile_req) {
            Ok(
                TileSource::NdpiJpeg { .. }
                | TileSource::NdpiFullDecode { .. }
                | TileSource::SyntheticDownsample { .. },
            ) => false,
            Ok(TileSource::TiledIfd { .. }) => true,
            Ok(_) => true,
            Err(_) => true,
        }
    }

    fn read_region_fastpath(
        &self,
        ctx: &mut crate::core::registry::SlideReadContext<'_>,
        req: &RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        let cache = ctx.tile_cache();
        let series = self
            .layout
            .dataset
            .scenes
            .get(req.scene.get())
            .and_then(|scene| scene.series.get(req.series.get()))?;
        let level = series.levels.get(req.level.get() as usize)?;
        if !matches!(level.tile_layout, TileLayout::WholeLevel { .. }) {
            return None;
        }
        let plane = req.plane.get();

        let source = self.layout.tile_sources.get(&TileSourceKey {
            scene: req.scene.get(),
            series: req.series.get(),
            level: req.level.get(),
            z: plane.z,
            c: plane.c,
            t: plane.t,
        })?;
        match source {
            TileSource::SyntheticDownsample { base_level, factor } => {
                Some(self.read_full_synthetic_region_fastpath(
                    cache,
                    req,
                    *base_level,
                    *factor,
                    ctx.max_region_pixels(),
                ))
            }
            _ => None,
        }
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        let source = self.tile_source_for(req)?;
        match source {
            TileSource::NdpiJpeg {
                ifd_id,
                jpeg_header,
                mcu_starts_tag,
                tiles_across,
                tiles_down,
                restart_interval,
                strip_offset,
                strip_byte_count,
            } => match self.read_ndpi_restart_tile(
                req,
                *ifd_id,
                jpeg_header,
                *mcu_starts_tag,
                *tiles_across,
                *tiles_down,
                *restart_interval,
                *strip_offset,
                *strip_byte_count,
            ) {
                Ok(tile) => Ok(tile),
                Err(err) if Self::ndpi_restart_error_allows_full_decode_fallback(&err) => self
                    .read_ndpi_full_decode_tile(
                        req,
                        *ifd_id,
                        jpeg_header,
                        *strip_offset,
                        *strip_byte_count,
                    ),
                Err(err) => Err(err),
            },
            TileSource::NdpiFullDecode {
                ifd_id,
                jpeg_header,
                strip_offset,
                strip_byte_count,
            } => self.read_ndpi_full_decode_tile(
                req,
                *ifd_id,
                jpeg_header,
                *strip_offset,
                *strip_byte_count,
            ),
            TileSource::TiledIfd {
                ifd_id,
                jpeg_tables,
                compression,
            } => self.read_tiled_ifd_tile(
                req,
                *ifd_id,
                jpeg_tables.as_deref(),
                *compression,
                BackendRequest::Auto,
            ),
            TileSource::SyntheticDownsample { base_level, factor } => {
                if req.col != 0 || req.row != 0 {
                    return Err(WsiError::TileRead {
                        col: req.col,
                        row: req.row,
                        level: req.level.get(),
                        reason: "synthetic NDPI whole-level tiles only support tile (0,0)".into(),
                    });
                }
                Ok(self
                    .get_or_decode_synthetic_level(req, *base_level, *factor)?
                    .as_ref()
                    .clone())
            }
            TileSource::StrippedLevel {
                ifd_id,
                compression,
                strip_offsets,
                strip_byte_counts,
            } => self.read_stripped_level_tile(
                req,
                *ifd_id,
                *compression,
                strip_offsets,
                strip_byte_counts,
            ),
            TileSource::Stripped { .. } => Err(WsiError::UnsupportedFormat(
                "Associated stripped images cannot be read via read_tile()".into(),
            )),
            TileSource::ExternalJpeg { .. } => Err(WsiError::UnsupportedFormat(
                "External JPEG associated images cannot be read via read_tile()".into(),
            )),
        }
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        let backend = output.backend().to_j2k();
        let require_device = output.requires_device();
        #[cfg(any(feature = "metal", feature = "cuda"))]
        let prefer_device = output.prefers_device();
        #[cfg(any(feature = "metal", feature = "cuda"))]
        let compressed_device_decode_enabled = output.compressed_device_decode_enabled();
        #[cfg(feature = "metal")]
        let metal_sessions = output.metal_sessions();
        #[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "metal")))]
        let metal_sessions = None;
        #[cfg(feature = "cuda")]
        let cuda_sessions = output.cuda_sessions();
        #[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "cuda")))]
        let cuda_sessions = None;

        #[cfg(any(feature = "metal", feature = "cuda"))]
        if prefer_device && !reqs.is_empty() {
            if self.ndpi_jpeg_batchable(reqs)? {
                if compressed_device_decode_enabled || jpeg_device_decode_enabled() {
                    match self.decode_ndpi_jpeg_pixels(
                        reqs,
                        backend,
                        require_device,
                        metal_sessions,
                        cuda_sessions,
                    ) {
                        Ok(tiles) => return Ok(tiles),
                        Err(err) if require_device => return Err(err),
                        Err(err) => {
                            tracing::debug!(
                                error = %err,
                                fallback_to_cpu = true,
                                fallback_reason = "ndpi_jpeg_device_decode_failed",
                                "NDPI JPEG device tile path failed; retrying through CPU output"
                            );
                        }
                    }
                } else if require_device {
                    return Err(WsiError::Unsupported {
                        reason: format!(
                            "NDPI JPEG device decode is disabled; set {JPEG_DEVICE_DECODE_ENV}=1 or request compressed device decode to opt in"
                        ),
                    });
                }
            }

            let device_result = match self.tiled_ifd_batch_compression(reqs)? {
                Some(Compression::Jpeg)
                    if compressed_device_decode_enabled || jpeg_device_decode_enabled() =>
                {
                    Some(self.decode_tiled_ifd_jpeg_pixels(
                        reqs,
                        backend,
                        require_device,
                        metal_sessions,
                        cuda_sessions,
                    ))
                }
                Some(Compression::Jpeg) if require_device => {
                    return Err(WsiError::Unsupported {
                        reason: format!(
                            "JPEG device decode is disabled; set {JPEG_DEVICE_DECODE_ENV}=1 or request compressed device decode to opt in"
                        ),
                    });
                }
                Some(Compression::Jpeg) => None,
                Some(compression @ (Compression::Jp2kRgb | Compression::Jp2kYcbcr))
                    if compressed_device_decode_enabled || jp2k_device_decode_enabled() =>
                {
                    Some(self.decode_tiled_ifd_jp2k_pixels(
                        reqs,
                        compression,
                        backend,
                        require_device,
                        metal_sessions,
                        cuda_sessions,
                    ))
                }
                Some(Compression::Jp2kRgb | Compression::Jp2kYcbcr) if require_device => {
                    return Err(WsiError::Unsupported {
                        reason: format!(
                            "JP2K device decode is disabled; set {JP2K_DEVICE_DECODE_ENV}=1 or request compressed device decode to opt in"
                        ),
                    });
                }
                Some(Compression::Jp2kRgb | Compression::Jp2kYcbcr) => None,
                _ if require_device => {
                    return Err(WsiError::Unsupported {
                        reason: "device backend not available for tiff_family".into(),
                    });
                }
                _ => None,
            };
            if let Some(result) = device_result {
                match result {
                    Ok(tiles) => return Ok(tiles),
                    Err(err) if require_device => return Err(err),
                    Err(err) => {
                        tracing::debug!(
                            error = %err,
                            fallback_to_cpu = true,
                            fallback_reason = "j2k_auto_chose_cpu",
                            "device tile path failed; retrying through CPU output"
                        );
                    }
                }
            }
        }

        #[cfg(not(any(feature = "metal", feature = "cuda")))]
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for tiff_family".into(),
            });
        }

        self.read_tiles_cpu_with_backend(reqs, backend)
            .map(|tiles| tiles.into_iter().map(TilePixels::Cpu).collect())
    }

    fn read_tiles_controlled(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
        control: &crate::ReadControl,
    ) -> Result<Vec<TilePixels>, WsiError> {
        control.check_cancelled()?;
        if !output.prefers_device() {
            let mut tiles = Vec::with_capacity(reqs.len());
            for request in reqs {
                control.check_cancelled()?;
                tiles.extend(self.read_tiles(std::slice::from_ref(request), output.clone())?);
                control.check_cancelled()?;
            }
            return Ok(tiles);
        }

        // Two tiles is the smallest batch that enables the resident Metal
        // JPEG path. This retains device batching while bounding how much work
        // can pass between cooperative cancellation checks.
        let mut tiles = Vec::with_capacity(reqs.len());
        for requests in reqs.chunks(2) {
            control.check_cancelled()?;
            let mut decoded = self.read_tiles(requests, output.clone())?;
            if decoded.len() != requests.len() {
                return Err(WsiError::TileRead {
                    col: requests[0].col,
                    row: requests[0].row,
                    level: requests[0].level.get(),
                    reason: format!(
                        "controlled TIFF batch returned {} tiles for {} requests",
                        decoded.len(),
                        requests.len()
                    ),
                });
            }
            tiles.append(&mut decoded);
            control.check_cancelled()?;
        }
        Ok(tiles)
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_cpu_with_backend(reqs, BackendRequest::Auto)
    }

    fn read_display_tile(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        let source = self.tile_source_for(&TileRequest {
            scene: req.scene.get().into(),
            series: req.series.get().into(),
            level: req.level.get().into(),
            plane: req.plane,
            col: req.col,
            row: req.row,
        })?;
        match source {
            TileSource::NdpiJpeg {
                ifd_id,
                jpeg_header,
                mcu_starts_tag,
                tiles_across,
                tiles_down,
                strip_offset,
                strip_byte_count,
                ..
            } => self.read_ndpi_display_tile(
                req,
                *ifd_id,
                jpeg_header,
                *mcu_starts_tag,
                *tiles_across,
                *tiles_down,
                *strip_offset,
                *strip_byte_count,
            ),
            TileSource::NdpiFullDecode {
                ifd_id,
                strip_offset,
                strip_byte_count,
                ..
            } => self.read_ndpi_full_display_tile(req, *ifd_id, *strip_offset, *strip_byte_count),
            TileSource::SyntheticDownsample { base_level, factor } => {
                self.read_synthetic_display_tile(req, *base_level, *factor)
            }
            _ => read_display_tile_from_source(self, None, req, TileOutputPreference::cpu()),
        }
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let source = self
            .layout
            .associated_sources
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;

        match source {
            TileSource::Stripped {
                ifd_id,
                jpeg_tables,
                compression,
                strip_offsets,
                strip_byte_counts,
            } => {
                let info = self
                    .layout
                    .dataset
                    .associated_images
                    .get(name)
                    .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;

                match compression {
                    Compression::Jpeg => self.read_stripped_jpeg_image(
                        name,
                        *ifd_id,
                        jpeg_tables.as_deref(),
                        info.dimensions,
                        strip_offsets,
                        strip_byte_counts,
                    ),
                    Compression::None => {
                        let data =
                            self.read_stripped_data(name, strip_offsets, strip_byte_counts)?;
                        self.decode_uncompressed_tile(
                            *ifd_id,
                            &data,
                            info.dimensions.0,
                            info.dimensions.1,
                        )
                    }
                    Compression::Lzw | Compression::Deflate | Compression::Zstd => {
                        let data =
                            self.read_stripped_data(name, strip_offsets, strip_byte_counts)?;
                        let expected_bytes = self.expected_uncompressed_tile_bytes(
                            *ifd_id,
                            info.dimensions.0,
                            info.dimensions.1,
                        )?;
                        let decoded = self.decompress_tiff_payload(
                            *ifd_id,
                            *compression,
                            &data,
                            expected_bytes,
                            info.dimensions.0,
                            info.dimensions.1,
                        )?;
                        self.decode_uncompressed_tile(
                            *ifd_id,
                            &decoded,
                            info.dimensions.0,
                            info.dimensions.1,
                        )
                    }
                    Compression::Jp2kRgb => {
                        let data =
                            self.read_stripped_data(name, strip_offsets, strip_byte_counts)?;
                        decode_one_jp2k(Jp2kDecodeJob {
                            data: Cow::Borrowed(&data),
                            expected_width: info.dimensions.0,
                            expected_height: info.dimensions.1,
                            rgb_color_space: true,
                            backend: BackendRequest::Auto,
                        })
                    }
                    Compression::Jp2kYcbcr => {
                        let data =
                            self.read_stripped_data(name, strip_offsets, strip_byte_counts)?;
                        decode_one_jp2k(Jp2kDecodeJob {
                            data: Cow::Borrowed(&data),
                            expected_width: info.dimensions.0,
                            expected_height: info.dimensions.1,
                            rgb_color_space: false,
                            backend: BackendRequest::Auto,
                        })
                    }
                    other => Err(WsiError::UnsupportedFormat(format!(
                        "associated image '{}' has unsupported compression {:?}",
                        name, other,
                    ))),
                }
            }
            TileSource::NdpiFullDecode {
                ifd_id,
                strip_offset,
                strip_byte_count,
                ..
            } => {
                let data = self
                    .container
                    .pread(*strip_offset, *strip_byte_count)
                    .map_err(|e| e.into_wsi_error(self.container.path()))?;

                let info = self
                    .layout
                    .dataset
                    .associated_images
                    .get(name)
                    .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;

                let options = j2k_decode_options(
                    self.tiff_jpeg_decode_options_for_data(*ifd_id, false, &data, None)
                        .color_transform,
                );
                let view = J2kJpegView::parse_with_options(&data, options)
                    .map_err(|err| WsiError::Jpeg(err.to_string()))?;
                let decoder = J2kJpegDecoder::from_view(view)
                    .map_err(|err| WsiError::Jpeg(err.to_string()))?;
                let (pixels, outcome) = decoder
                    .decode_request(J2kJpegDecodeRequest::full(J2kPixelFormat::Rgb8))
                    .map_err(|err| WsiError::Jpeg(err.to_string()))?;
                let decoded =
                    cpu_tile_from_rgb_pixels(outcome.decoded.w, outcome.decoded.h, pixels)?;
                if decoded.width > info.dimensions.0 || decoded.height > info.dimensions.1 {
                    crop_rgb_interleaved_u8_buffer(
                        &decoded,
                        0,
                        0,
                        info.dimensions.0,
                        info.dimensions.1,
                    )
                } else {
                    Ok(decoded)
                }
            }
            TileSource::TiledIfd {
                ifd_id,
                jpeg_tables,
                compression,
            } => {
                let info = self
                    .layout
                    .dataset
                    .associated_images
                    .get(name)
                    .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
                self.read_tiled_associated_image(
                    name,
                    *ifd_id,
                    jpeg_tables.as_deref(),
                    *compression,
                    info.dimensions,
                )
            }
            TileSource::ExternalJpeg { path } => {
                let data = std::fs::read(path).map_err(|err| WsiError::InvalidSlide {
                    path: path.clone(),
                    message: format!(
                        "failed to read external JPEG associated image '{}': {err}",
                        path.display()
                    ),
                })?;
                decode_one_jpeg(JpegDecodeJob {
                    data: Cow::Borrowed(&data),
                    tables: None,
                    expected_width: 0,
                    expected_height: 0,
                    color_transform: J2kColorTransform::Auto,
                    force_dimensions: false,
                    requested_size: None,
                })
            }
            _ => Err(WsiError::UnsupportedFormat(format!(
                "associated image '{}' has unsupported source type",
                name,
            ))),
        }
    }

    fn recommended_shared_cache_bytes(&self) -> Option<u64> {
        self.layout
            .tile_sources
            .values()
            .any(|source| {
                matches!(
                    source,
                    TileSource::TiledIfd {
                        compression: Compression::Jp2kRgb | Compression::Jp2kYcbcr,
                        ..
                    }
                )
            })
            .then_some(DEFAULT_JP2K_SHARED_TILE_CACHE_BYTES)
    }
}
