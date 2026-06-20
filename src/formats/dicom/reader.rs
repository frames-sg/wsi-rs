use super::*;

pub(super) struct DicomReader {
    pub(super) slide: Arc<DicomSlide>,
}

impl SlideReader for DicomReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.slide
            .levels
            .get(req.level.get() as usize)
            .map(|level| level.tile_codec_kind(req))
            .unwrap_or(TileCodecKind::Other)
    }

    fn use_display_tile_cache(&self, _req: &TileViewRequest) -> bool {
        true
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        let backend = output.backend().to_j2k();

        #[cfg(any(feature = "metal", feature = "cuda"))]
        if output.prefers_device() {
            match self.read_tiles_jp2k_device_batch(reqs, &output, backend) {
                Ok(Some(tiles)) => return Ok(tiles),
                Ok(None) => {}
                Err(err) if output.requires_device() => return Err(err),
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        fallback_to_cpu = true,
                        fallback_reason = "dicom_jp2k_device_batch_failed",
                        "DICOM JP2K device batch failed; retrying through CPU output"
                    );
                }
            }
            match self.read_tiles_jpeg_device_batch(reqs, &output, backend) {
                Ok(Some(tiles)) => return Ok(tiles),
                Ok(None) => {}
                Err(err) if output.requires_device() => return Err(err),
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        fallback_to_cpu = true,
                        fallback_reason = "dicom_jpeg_device_batch_failed",
                        "DICOM JPEG device batch failed; retrying through CPU output"
                    );
                }
            }
        }

        if output.requires_device() {
            return Err(WsiError::Unsupported {
                reason: "RequireDevice not supported for DICOM CPU fallback".into(),
            });
        }

        self.read_tiles_cpu_with_backend(reqs, backend)
            .map(|tiles| tiles.into_iter().map(TilePixels::Cpu).collect())
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Auto)
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        let image =
            self.slide
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                })?;
        image.read_raw_compressed_tile(req.col, req.row, req.level.get())
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let image = self
            .slide
            .associated
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        image.read_associated(name)
    }

    fn recommended_shared_cache_bytes(&self) -> Option<u64> {
        self.slide.recommended_shared_cache_bytes()
    }
}

#[derive(Clone)]
struct DicomCpuBatchMeta {
    slot: usize,
    req: TileRequest,
    image: Arc<DicomImage>,
    frame_index: u32,
    actual_width: u32,
    actual_height: u32,
    cache_decoded_frame: bool,
}

type DicomCpuFrameBytes = (DicomCpuBatchMeta, Arc<Vec<u8>>);

fn attach_encapsulated_frame_bytes(
    metas: Vec<DicomCpuBatchMeta>,
    cache_result: bool,
) -> Result<Vec<DicomCpuFrameBytes>, WsiError> {
    let mut groups: HashMap<usize, (Arc<DicomImage>, Vec<DicomCpuBatchMeta>)> = HashMap::new();
    for meta in metas {
        let key = Arc::as_ptr(&meta.image) as usize;
        groups
            .entry(key)
            .or_insert_with(|| (meta.image.clone(), Vec::new()))
            .1
            .push(meta);
    }

    let mut jobs = Vec::new();
    for (_, (image, mut metas)) in groups {
        let cache_decoded_frame = image.should_cache_decoded_frames_for_batch(metas.len());
        for meta in &mut metas {
            meta.cache_decoded_frame = cache_decoded_frame;
        }
        let frame_indices = metas
            .iter()
            .map(|meta| meta.frame_index)
            .collect::<Vec<_>>();
        let first = &metas[0].req;
        let frames = image.extract_encapsulated_frames(
            &frame_indices,
            first.level.get(),
            first.col,
            first.row,
            cache_result,
        )?;
        for meta in metas {
            let bytes =
                frames
                    .get(&meta.frame_index)
                    .cloned()
                    .ok_or_else(|| WsiError::TileRead {
                        col: meta.req.col,
                        row: meta.req.row,
                        level: meta.req.level.get(),
                        reason: format!("DICOM batch frame {} was not extracted", meta.frame_index),
                    })?;
            jobs.push((meta, bytes));
        }
    }
    jobs.sort_by_key(|(meta, _)| meta.slot);
    Ok(jobs)
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) struct DicomDeviceDecodeJob {
    pub(super) slot: usize,
    pub(super) req: TileRequest,
    pub(super) image: Arc<DicomImage>,
    pub(super) frame_index: u32,
    pub(super) cache_decoded_frame: bool,
}

#[cfg(any(feature = "metal", feature = "cuda"))]
type DicomDeviceFrameBytes = (DicomDeviceDecodeJob, Arc<Vec<u8>>);

#[cfg(any(feature = "metal", feature = "cuda"))]
fn attach_device_encapsulated_frame_bytes(
    metas: Vec<DicomDeviceDecodeJob>,
    cache_result: bool,
) -> Result<Vec<DicomDeviceFrameBytes>, WsiError> {
    let mut groups: HashMap<usize, (Arc<DicomImage>, Vec<DicomDeviceDecodeJob>)> = HashMap::new();
    for meta in metas {
        let key = Arc::as_ptr(&meta.image) as usize;
        groups
            .entry(key)
            .or_insert_with(|| (meta.image.clone(), Vec::new()))
            .1
            .push(meta);
    }

    let mut jobs = Vec::new();
    for (_, (image, mut metas)) in groups {
        let cache_decoded_frame = image.should_cache_decoded_frames_for_batch(metas.len());
        for meta in &mut metas {
            meta.cache_decoded_frame = cache_decoded_frame;
        }
        let frame_indices = metas
            .iter()
            .map(|meta| meta.frame_index)
            .collect::<Vec<_>>();
        let first = &metas[0].req;
        let frames = image.extract_encapsulated_frames(
            &frame_indices,
            first.level.get(),
            first.col,
            first.row,
            cache_result,
        )?;
        for meta in metas {
            let bytes =
                frames
                    .get(&meta.frame_index)
                    .cloned()
                    .ok_or_else(|| WsiError::TileRead {
                        col: meta.req.col,
                        row: meta.req.row,
                        level: meta.req.level.get(),
                        reason: format!(
                            "DICOM device batch frame {} was not extracted",
                            meta.frame_index
                        ),
                    })?;
            jobs.push((meta, bytes));
        }
    }
    jobs.sort_by_key(|(meta, _)| meta.slot);
    Ok(jobs)
}

#[cfg(any(feature = "metal", feature = "cuda"))]
impl DicomReader {
    pub(super) fn read_tiles_jp2k_device_batch(
        &self,
        reqs: &[TileRequest],
        output: &TileOutputPreference,
        backend: BackendRequest,
    ) -> Result<Option<Vec<TilePixels>>, WsiError> {
        if reqs.is_empty() {
            return Ok(Some(Vec::new()));
        }
        if !output.compressed_device_decode_enabled() && !dicom_jp2k_device_decode_enabled() {
            return Ok(None);
        }
        #[cfg(feature = "metal")]
        let metal_sessions = output.metal_sessions();
        #[cfg(not(feature = "metal"))]
        let metal_sessions = None;
        #[cfg(feature = "cuda")]
        let cuda_sessions = output.cuda_sessions();
        #[cfg(not(feature = "cuda"))]
        let cuda_sessions = None;
        if metal_sessions.is_none() && cuda_sessions.is_none() {
            if output.requires_device() {
                return Err(WsiError::Unsupported {
                    reason:
                        "device backend not available for DICOM JP2K without Metal or CUDA session"
                            .into(),
                });
            }
            return Ok(None);
        }

        let mut results: Vec<Option<TilePixels>> = Vec::with_capacity(reqs.len());
        results.resize_with(reqs.len(), || None);
        let mut job_meta = Vec::new();
        let mut saw_device_candidate = false;

        for (slot, req) in reqs.iter().enumerate() {
            let level = self.slide.levels.get(req.level.get() as usize).ok_or(
                WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                },
            )?;
            if req.col < 0
                || req.row < 0
                || req.col >= level.tiles_across as i64
                || req.row >= level.tiles_down as i64
            {
                return Err(WsiError::Unsupported {
                    reason: format!(
                        "tile ({},{}) out of range for DICOM device decode",
                        req.col, req.row
                    ),
                });
            }

            let col = req.col as u32;
            let row = req.row as u32;
            let Some(image) = level.image_for_tile(col, row) else {
                if output.requires_device() {
                    return Err(WsiError::Unsupported {
                        reason:
                            "DICOM device batch cannot return CPU black tile for sparse missing tile"
                                .into(),
                    });
                }
                let (width, height) = level.actual_tile_dimensions(col, row);
                results[slot] = Some(TilePixels::Cpu(black_sample_buffer(width, height)));
                continue;
            };
            if !dicom_jp2k_device_batch_allowed(
                image.transfer_syntax_uid.as_str(),
                output,
                reqs.len(),
            ) {
                continue;
            }
            let Some(frame_index) = image.frame_index(col, row) else {
                if output.requires_device() {
                    return Err(WsiError::Unsupported {
                        reason:
                            "DICOM device batch cannot return CPU black tile for sparse missing tile"
                                .into(),
                    });
                }
                let (width, height) = level.actual_tile_dimensions(col, row);
                results[slot] = Some(TilePixels::Cpu(black_sample_buffer(width, height)));
                continue;
            };
            let (actual_width, actual_height) = level.actual_tile_dimensions(col, row);
            if actual_width != image.tile_width || actual_height != image.tile_height {
                continue;
            }
            if image.samples_per_pixel != 3 {
                continue;
            }

            saw_device_candidate = true;
            if !output.requires_device() {
                if let Some(cached) = image.cached_decoded_frame(frame_index) {
                    results[slot] = Some(TilePixels::Cpu(cached.as_ref().clone()));
                    continue;
                }
            }

            job_meta.push(DicomDeviceDecodeJob {
                slot,
                req: req.clone(),
                image: image.clone(),
                frame_index,
                cache_decoded_frame: true,
            });
        }

        if job_meta.is_empty() && !saw_device_candidate {
            return Ok(None);
        }
        if job_meta.is_empty() {
            return results
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .map(Some)
                .ok_or_else(|| WsiError::Unsupported {
                    reason: "DICOM device batch had no decodable JP2K frames".into(),
                });
        }

        let job_meta = attach_device_encapsulated_frame_bytes(job_meta, true)?;
        let jobs = job_meta
            .iter()
            .map(|(meta, bytes)| Jp2kDecodeJob {
                data: Cow::Owned(bytes.as_ref().clone()),
                expected_width: meta.image.tile_width,
                expected_height: meta.image.tile_height,
                rgb_color_space: !jp2k_photometric_is_ycbcr(
                    meta.image.photometric_interpretation.as_str(),
                ),
                backend,
            })
            .collect::<Vec<_>>();
        let decoded = decode_batch_jp2k_pixels(
            &jobs,
            output.requires_device(),
            metal_sessions,
            cuda_sessions,
        );
        if decoded.len() != job_meta.len() {
            return Err(WsiError::Jp2k(format!(
                "DICOM JP2K device batch returned {} tiles for {} jobs",
                decoded.len(),
                job_meta.len()
            )));
        }

        for ((meta, _), decoded) in job_meta.into_iter().zip(decoded) {
            let tile = decoded?;
            if meta.cache_decoded_frame {
                if let TilePixels::Cpu(cpu) = &tile {
                    meta.image
                        .cache_decoded_frame(meta.frame_index, Arc::new(cpu.clone()));
                }
            }
            results[meta.slot] = Some(tile);
        }

        for (slot, result) in results.iter_mut().enumerate() {
            if result.is_none() {
                if output.requires_device() {
                    return Err(WsiError::Unsupported {
                        reason: "DICOM device batch contained a non-device-decodable tile".into(),
                    });
                }
                *result = Some(TilePixels::Cpu(
                    self.read_tile_with_backend(&reqs[slot], backend)?,
                ));
            }
        }

        Ok(Some(
            results
                .into_iter()
                .map(|tile| {
                    tile.ok_or_else(|| WsiError::TileRead {
                        col: 0,
                        row: 0,
                        level: 0u32,
                        reason: "DICOM device batch result was not populated".into(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }

    pub(super) fn read_tiles_jpeg_device_batch(
        &self,
        reqs: &[TileRequest],
        output: &TileOutputPreference,
        backend: BackendRequest,
    ) -> Result<Option<Vec<TilePixels>>, WsiError> {
        if reqs.is_empty() {
            return Ok(Some(Vec::new()));
        }
        if !output.compressed_device_decode_enabled() {
            return Ok(None);
        }
        #[cfg(feature = "metal")]
        let metal_sessions = output.metal_sessions();
        #[cfg(not(feature = "metal"))]
        let metal_sessions = None;
        #[cfg(feature = "cuda")]
        let cuda_sessions = output.cuda_sessions();
        #[cfg(not(feature = "cuda"))]
        let cuda_sessions = None;
        if metal_sessions.is_none() && cuda_sessions.is_none() {
            if output.requires_device() {
                return Err(WsiError::Unsupported {
                    reason:
                        "device backend not available for DICOM JPEG without Metal or CUDA session"
                            .into(),
                });
            }
            return Ok(None);
        }

        let mut results: Vec<Option<TilePixels>> = Vec::with_capacity(reqs.len());
        results.resize_with(reqs.len(), || None);
        let mut job_meta = Vec::new();
        let mut saw_device_candidate = false;

        for (slot, req) in reqs.iter().enumerate() {
            let level = self.slide.levels.get(req.level.get() as usize).ok_or(
                WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                },
            )?;
            if req.col < 0
                || req.row < 0
                || req.col >= level.tiles_across as i64
                || req.row >= level.tiles_down as i64
            {
                return Err(WsiError::Unsupported {
                    reason: format!(
                        "tile ({},{}) out of range for DICOM device decode",
                        req.col, req.row
                    ),
                });
            }

            let col = req.col as u32;
            let row = req.row as u32;
            let Some(image) = level.image_for_tile(col, row) else {
                if output.requires_device() {
                    return Err(WsiError::Unsupported {
                        reason:
                            "DICOM device batch cannot return CPU black tile for sparse missing tile"
                                .into(),
                    });
                }
                let (width, height) = level.actual_tile_dimensions(col, row);
                results[slot] = Some(TilePixels::Cpu(black_sample_buffer(width, height)));
                continue;
            };
            if image.transfer_syntax_uid != JPEG_TRANSFER_SYNTAX {
                continue;
            }
            let Some(frame_index) = image.frame_index(col, row) else {
                if output.requires_device() {
                    return Err(WsiError::Unsupported {
                        reason:
                            "DICOM device batch cannot return CPU black tile for sparse missing tile"
                                .into(),
                    });
                }
                let (width, height) = level.actual_tile_dimensions(col, row);
                results[slot] = Some(TilePixels::Cpu(black_sample_buffer(width, height)));
                continue;
            };
            let (actual_width, actual_height) = level.actual_tile_dimensions(col, row);
            if actual_width != image.tile_width || actual_height != image.tile_height {
                continue;
            }
            if image.samples_per_pixel != 3 {
                continue;
            }

            saw_device_candidate = true;
            if !output.requires_device() {
                if let Some(cached) = image.cached_decoded_frame(frame_index) {
                    results[slot] = Some(TilePixels::Cpu(cached.as_ref().clone()));
                    continue;
                }
            }

            job_meta.push(DicomDeviceDecodeJob {
                slot,
                req: req.clone(),
                image: image.clone(),
                frame_index,
                cache_decoded_frame: true,
            });
        }

        if job_meta.is_empty() && !saw_device_candidate {
            return Ok(None);
        }
        if job_meta.is_empty() {
            return results
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .map(Some)
                .ok_or_else(|| WsiError::Unsupported {
                    reason: "DICOM device batch had no decodable JPEG frames".into(),
                });
        }

        let job_meta = attach_device_encapsulated_frame_bytes(job_meta, true)?;
        let jobs = job_meta
            .iter()
            .map(|(meta, bytes)| JpegDecodeJob {
                data: Cow::Owned(bytes.as_ref().clone()),
                tables: None,
                expected_width: meta.image.tile_width,
                expected_height: meta.image.tile_height,
                color_transform: j2k_jpeg::ColorTransform::Auto,
                force_dimensions: false,
                requested_size: None,
            })
            .collect::<Vec<_>>();
        let decoded = decode_batch_jpeg_pixels(
            &jobs,
            backend,
            output.requires_device(),
            metal_sessions,
            cuda_sessions,
        );
        if decoded.len() != job_meta.len() {
            return Err(WsiError::Jpeg(format!(
                "DICOM JPEG device batch returned {} tiles for {} jobs",
                decoded.len(),
                job_meta.len()
            )));
        }

        for ((meta, _), decoded) in job_meta.into_iter().zip(decoded) {
            let tile = decoded?;
            if meta.cache_decoded_frame {
                if let TilePixels::Cpu(cpu) = &tile {
                    meta.image
                        .cache_decoded_frame(meta.frame_index, Arc::new(cpu.clone()));
                }
            }
            results[meta.slot] = Some(tile);
        }

        for (slot, result) in results.iter_mut().enumerate() {
            if result.is_none() {
                if output.requires_device() {
                    return Err(WsiError::Unsupported {
                        reason: "DICOM device batch contained a non-device-decodable tile".into(),
                    });
                }
                *result = Some(TilePixels::Cpu(
                    self.read_tile_with_backend(&reqs[slot], backend)?,
                ));
            }
        }

        Ok(Some(
            results
                .into_iter()
                .map(|tile| {
                    tile.ok_or_else(|| WsiError::TileRead {
                        col: 0,
                        row: 0,
                        level: 0u32,
                        reason: "DICOM device batch result was not populated".into(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }
}

pub(super) fn dicom_tile_codec_kind(transfer_syntax_uid: &str) -> TileCodecKind {
    if transfer_syntax_uid == JPEG_TRANSFER_SYNTAX {
        TileCodecKind::Jpeg
    } else if matches!(
        transfer_syntax_uid,
        HTJ2K_TRANSFER_SYNTAX
            | HTJ2K_LOSSLESS_TRANSFER_SYNTAX
            | HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX
    ) {
        TileCodecKind::Htj2k
    } else if JP2K_TRANSFER_SYNTAXES.contains(&transfer_syntax_uid) {
        TileCodecKind::Jp2k
    } else {
        TileCodecKind::Other
    }
}

impl DicomReader {
    pub(super) fn read_tiles_cpu_with_backend(
        &self,
        reqs: &[TileRequest],
        backend: BackendRequest,
    ) -> Result<Vec<CpuTile>, WsiError> {
        if reqs.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = vec![None; reqs.len()];
        let mut jpeg_metas = Vec::new();
        let mut jp2k_metas = Vec::new();
        let mut rle_metas = Vec::new();

        for (slot, req) in reqs.iter().enumerate() {
            let level = self.slide.levels.get(req.level.get() as usize).ok_or(
                WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                },
            )?;
            if req.col < 0
                || req.row < 0
                || req.col >= level.tiles_across as i64
                || req.row >= level.tiles_down as i64
            {
                return Err(WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: format!(
                        "tile ({},{}) out of range ({}x{})",
                        req.col, req.row, level.tiles_across, level.tiles_down
                    ),
                });
            }

            let col = req.col as u32;
            let row = req.row as u32;
            let Some(image) = level.image_for_tile(col, row) else {
                let (width, height) = level.actual_tile_dimensions(col, row);
                results[slot] = Some(black_sample_buffer(width, height));
                continue;
            };
            let Some(frame_index) = image.frame_index(col, row) else {
                let (width, height) = level.actual_tile_dimensions(col, row);
                results[slot] = Some(black_sample_buffer(width, height));
                continue;
            };
            let (actual_width, actual_height) = level.actual_tile_dimensions(col, row);

            if is_encapsulated_transfer_syntax(&image.transfer_syntax_uid) {
                if let Some(cached) = image.cached_decoded_frame(frame_index) {
                    results[slot] = Some(crop_sample_buffer_rgb(
                        cached.as_ref(),
                        actual_width,
                        actual_height,
                    )?);
                    continue;
                }
            }

            let meta = DicomCpuBatchMeta {
                slot,
                req: req.clone(),
                image: image.clone(),
                frame_index,
                actual_width,
                actual_height,
                cache_decoded_frame: true,
            };
            if image.transfer_syntax_uid == JPEG_TRANSFER_SYNTAX {
                jpeg_metas.push(meta);
            } else if JP2K_TRANSFER_SYNTAXES.contains(&image.transfer_syntax_uid.as_str()) {
                jp2k_metas.push(meta);
            } else if image.transfer_syntax_uid == RLE_TRANSFER_SYNTAX {
                rle_metas.push(meta);
            } else {
                results[slot] = Some(self.read_tile_with_backend(req, backend)?);
            }
        }

        let jpeg_jobs = attach_encapsulated_frame_bytes(jpeg_metas, false)?;
        let jpeg_decode_jobs = jpeg_jobs
            .iter()
            .map(|(meta, bytes)| JpegDecodeJob {
                data: Cow::Borrowed(bytes.as_slice()),
                tables: None,
                expected_width: meta.image.tile_width,
                expected_height: meta.image.tile_height,
                color_transform: j2k_jpeg::ColorTransform::Auto,
                force_dimensions: false,
                requested_size: None,
            })
            .collect::<Vec<_>>();
        let jpeg_decoded = decode_batch_jpeg(&jpeg_decode_jobs);
        for ((meta, _), decoded) in jpeg_jobs.into_iter().zip(jpeg_decoded) {
            let tile = decoded.map_err(|err| WsiError::TileRead {
                col: meta.req.col,
                row: meta.req.row,
                level: meta.req.level.get(),
                reason: err.to_string(),
            })?;
            if meta.cache_decoded_frame {
                meta.image
                    .cache_decoded_frame(meta.frame_index, Arc::new(tile.clone()));
            }
            results[meta.slot] = Some(crop_or_keep_sample_buffer_rgb(
                tile,
                meta.actual_width,
                meta.actual_height,
            )?);
        }

        let jp2k_jobs = attach_encapsulated_frame_bytes(jp2k_metas, false)?;
        let jp2k_decode_jobs = jp2k_jobs
            .iter()
            .map(|(meta, bytes)| Jp2kDecodeJob {
                data: Cow::Borrowed(bytes.as_slice()),
                expected_width: meta.image.tile_width,
                expected_height: meta.image.tile_height,
                rgb_color_space: !jp2k_photometric_is_ycbcr(
                    meta.image.photometric_interpretation.as_str(),
                ),
                backend,
            })
            .collect::<Vec<_>>();
        let jp2k_decoded = decode_batch_jp2k(&jp2k_decode_jobs);
        for ((meta, _), decoded) in jp2k_jobs.into_iter().zip(jp2k_decoded) {
            let tile = decoded.map_err(|err| WsiError::TileRead {
                col: meta.req.col,
                row: meta.req.row,
                level: meta.req.level.get(),
                reason: err.to_string(),
            })?;
            if meta.cache_decoded_frame {
                meta.image
                    .cache_decoded_frame(meta.frame_index, Arc::new(tile.clone()));
            }
            results[meta.slot] = Some(crop_or_keep_sample_buffer_rgb(
                tile,
                meta.actual_width,
                meta.actual_height,
            )?);
        }

        let rle_jobs = attach_encapsulated_frame_bytes(rle_metas, false)?;
        for (meta, bytes) in rle_jobs {
            let tile = decode_rle_lossless_frame(
                bytes.as_slice(),
                meta.image.tile_width,
                meta.image.tile_height,
                meta.image.samples_per_pixel,
                &meta.image.photometric_interpretation,
            )
            .map_err(|err| WsiError::TileRead {
                col: meta.req.col,
                row: meta.req.row,
                level: meta.req.level.get(),
                reason: err.to_string(),
            })?;
            if meta.cache_decoded_frame {
                meta.image
                    .cache_decoded_frame(meta.frame_index, Arc::new(tile.clone()));
            }
            results[meta.slot] = Some(crop_or_keep_sample_buffer_rgb(
                tile,
                meta.actual_width,
                meta.actual_height,
            )?);
        }

        results
            .into_iter()
            .zip(reqs.iter())
            .map(|(tile, req)| {
                tile.ok_or_else(|| WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: "DICOM CPU batch result was not populated".into(),
                })
            })
            .collect()
    }

    pub(super) fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let image =
            self.slide
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                })?;
        image.read_tile(req.col, req.row, req.level.get(), backend)
    }
}
