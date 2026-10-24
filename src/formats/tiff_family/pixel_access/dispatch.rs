use super::ndpi_tiles::NdpiRestartTile;
use super::*;

impl TiffPixelReader {
    /// Look up the TileSource for a given tile request.
    pub(super) fn tile_source_for(&self, req: &TileRequest) -> Result<&TileSource, WsiError> {
        self.layout
            .tile_sources
            .get(&TileSourceKey::from(req))
            .ok_or_else(|| {
                WsiError::tile_read(
                    req,
                    format!(
                        "no tile source for scene={}, series={}, level={}, z={}, c={}, t={}",
                        req.scene.get(),
                        req.series.get(),
                        req.level.get(),
                        req.plane.get().z,
                        req.plane.get().c,
                        req.plane.get().t,
                    ),
                )
            })
    }

    fn display_request_matches_native_regular_grid(&self, req: &TileViewRequest) -> bool {
        let Some(level) = self
            .layout
            .dataset
            .find_level(req.scene, req.series, req.level)
        else {
            return false;
        };
        let TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } = &level.tile_layout
        else {
            return false;
        };
        let (Ok(col), Ok(row)) = (u64::try_from(req.col), u64::try_from(req.row)) else {
            return false;
        };
        req.tile_width == *tile_width
            && req.tile_height == *tile_height
            && col < *tiles_across
            && row < *tiles_down
    }

    pub(super) fn read_tiles_cpu_with_backend(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
    ) -> Result<Vec<CpuTile>, WsiError> {
        if reqs.len() < 2 {
            return self.read_unique_tiles_cpu_with_backend(reqs, backend);
        }

        let mut first_slots = HashMap::with_capacity(reqs.len());
        let mut unique_indices = Vec::with_capacity(reqs.len());
        let mut output_slots = Vec::with_capacity(reqs.len());
        for (index, req) in reqs.iter().enumerate() {
            let key = (
                req.scene, req.series, req.level, req.plane, req.col, req.row,
            );
            let slot = *first_slots.entry(key).or_insert_with(|| {
                let slot = unique_indices.len();
                unique_indices.push(index);
                slot
            });
            output_slots.push(slot);
        }
        if unique_indices.len() == reqs.len() {
            return self.read_unique_tiles_cpu_with_backend(reqs, backend);
        }

        let unique_requests: Vec<_> = unique_indices
            .into_iter()
            .map(|index| reqs[index].clone())
            .collect();
        let tiles = crate::core::batch::expect_exact_count(
            self.read_unique_tiles_cpu_with_backend(&unique_requests, backend)?,
            unique_requests.len(),
            "TIFF unique CPU tile batch",
        )?;
        Ok(output_slots
            .into_iter()
            .map(|slot| tiles[slot].clone())
            .collect())
    }

    fn read_unique_tiles_cpu_with_backend(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
    ) -> Result<Vec<CpuTile>, WsiError> {
        if reqs.is_empty() {
            return Ok(Vec::new());
        }

        let first_source = self.tile_source_for(&reqs[0])?;
        if matches!(first_source, TileSource::TiledIfdSubtiles { .. }) {
            return self.read_tiled_ifd_subtiles_grouped(reqs, backend);
        }
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
                WsiError::tile_read(first, err.to_string())
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

    fn tile_is_present(&self, req: &TileRequest) -> Result<Option<bool>, WsiError> {
        match self.tile_source_for(req)? {
            TileSource::TiledIfd { ifd_id, .. } => Ok(Some(
                self.tiled_ifd_tile_span(req, *ifd_id)?.byte_count != 0,
            )),
            _ => Ok(None),
        }
    }

    fn associated_encoding(&self, name: &str) -> Result<Option<SourceEncoding>, WsiError> {
        let decoded = self
            .layout
            .dataset
            .associated_images
            .get(name)
            .and_then(|image| image.decoded_bytes());
        let (Some(decoded), Some(source)) = (decoded, self.layout.associated_sources.get(name))
        else {
            return Ok(None);
        };
        let (compression, encoded) = match source {
            TileSource::Stripped {
                compression,
                strip_byte_counts,
                ..
            } => (*compression, strip_byte_counts.iter().copied().sum::<u64>()),
            TileSource::NdpiFullDecode {
                strip_byte_count, ..
            } => (Compression::Jpeg, *strip_byte_count),
            TileSource::TiledIfd {
                ifd_id,
                compression,
                ..
            } => (
                *compression,
                self.container
                    .get_u64_array(*ifd_id, tags::TILE_BYTE_COUNTS)
                    .map_err(|e| e.into_wsi_error(self.container.path()))?
                    .iter()
                    .copied()
                    .sum::<u64>(),
            ),
            _ => return Ok(None),
        };
        Ok(Some(
            SourceEncoding::new(compression).with_sizes(decoded, encoded),
        ))
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
        self.layout.dataset.level(scene, series, level)?;
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
            TileSource::TiledIfdSubtiles { .. } => Err(WsiError::Unsupported {
                reason: "compressed passthrough is not available for Ventana subtiles cropped from stored tiles".into(),
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
        let tile_req = req.source_tile();
        match self.tile_source_for(&tile_req)? {
            TileSource::TiledIfd {
                ifd_id,
                jpeg_tables,
                compression: Compression::Jpeg,
            } => {
                if !self.display_request_matches_native_regular_grid(req) {
                    return Err(WsiError::Unsupported {
                        reason: "raw TIFF JPEG display passthrough requires the display grid to exactly match the native regular tile layout"
                            .into(),
                    });
                }
                self.read_tiled_ifd_raw_jpeg_tile(
                    &tile_req,
                    *ifd_id,
                    jpeg_tables.as_deref(),
                )
            }
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
            TileSource::TiledIfd { .. } => Err(WsiError::Unsupported {
                reason: "raw compressed TIFF display tile access requires JPEG compression".into(),
            }),
            _ => Err(WsiError::Unsupported {
                reason: "raw compressed display tile access is not available for this TIFF level"
                    .into(),
            }),
        }
    }

    fn use_display_tile_cache(&self, req: &TileViewRequest) -> bool {
        let tile_req = req.source_tile();
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
        let level = self
            .layout
            .dataset
            .find_level(req.scene, req.series, req.level)?;
        if !matches!(level.tile_layout, TileLayout::WholeLevel { .. }) {
            return None;
        }
        let source = self
            .layout
            .tile_sources
            .get(&TileSourceKey::from(&req.tile_at(0, 0)))?;
        match source {
            TileSource::NdpiJpeg { .. } => {
                let TileLayout::WholeLevel {
                    virtual_tile_width,
                    virtual_tile_height,
                    ..
                } = level.tile_layout
                else {
                    return None;
                };
                // Resolve cached strips on the caller; an incomplete region
                // composes and decodes on one pool worker.
                self.read_ndpi_region(ctx, req, (virtual_tile_width, virtual_tile_height))
            }
            // Scaled strips decode on the calling thread.
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
            )? {
                NdpiRestartTile::Decoded(tile) => Ok(tile),
                NdpiRestartTile::NeedsFullDecode => self.read_ndpi_full_decode_tile(
                    req,
                    *ifd_id,
                    jpeg_header,
                    *strip_offset,
                    *strip_byte_count,
                ),
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
                validate_tile_coords(req.col, req.row, req.level.get())?;
                let (_, _, level) = self
                    .layout
                    .dataset
                    .level(req.scene, req.series, req.level)?;
                let TileLayout::WholeLevel {
                    virtual_tile_width,
                    virtual_tile_height,
                    ..
                } = level.tile_layout
                else {
                    return Err(WsiError::UnsupportedFormat(
                        "synthetic NDPI requires a virtual tile grid".into(),
                    ));
                };
                self.read_synthetic_display_tile(
                    &TileViewRequest {
                        scene: req.scene,
                        series: req.series,
                        level: req.level,
                        plane: req.plane,
                        col: req.col,
                        row: req.row,
                        tile_width: virtual_tile_width,
                        tile_height: virtual_tile_height,
                    },
                    *base_level,
                    *factor,
                )
            }
            TileSource::TiledIfdSubtiles {
                ifd_id,
                jpeg_tables,
                compression,
                subtiles_per_tile,
            } => self.read_tiled_ifd_subtile(
                req,
                *ifd_id,
                jpeg_tables.as_deref(),
                *compression,
                *subtiles_per_tile,
            ),
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

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_cpu_with_backend(reqs, BackendRequest::Cpu)
    }

    #[cfg(feature = "metal")]
    fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        self.decode_tiled_ifd_jp2k_metal(reqs, sessions)
            .and_then(|tiles| {
                crate::core::batch::expect_exact_count(tiles, reqs.len(), "TIFF Metal tile batch")
            })
    }

    #[cfg(feature = "cuda")]
    fn read_tiles_cuda(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        self.decode_tiled_ifd_jp2k_cuda(reqs, sessions)
            .and_then(|tiles| {
                crate::core::batch::expect_exact_count(tiles, reqs.len(), "TIFF CUDA tile batch")
            })
    }

    fn read_display_tile(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        let source = self.tile_source_for(&req.source_tile())?;
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
            _ => read_display_tile_from_source(self, None, req),
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
                        let decoded = self.read_stripped_compressed_data(
                            name,
                            *ifd_id,
                            *compression,
                            info.dimensions,
                            strip_offsets,
                            strip_byte_counts,
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
                let data = read_file_bounded(
                    path,
                    MAX_COMPRESSED_INPUT_BYTES,
                    "external TIFF JPEG associated image",
                )
                .map_err(|err| WsiError::InvalidSlide {
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
}
