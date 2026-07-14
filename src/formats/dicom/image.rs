use super::*;

pub(super) const BATCH_FRAME_READ_MAX_SPAN_BYTES: u64 = 32 * 1024 * 1024;
pub(super) const BATCH_FRAME_READ_MAX_GAP_BYTES: u64 = 64 * 1024;

#[derive(Debug)]
pub(super) struct DicomImage {
    pub(super) path: PathBuf,
    pub(super) sop_instance_uid: String,
    pub(super) transfer_syntax_uid: String,
    pub(super) photometric_interpretation: String,
    pub(super) samples_per_pixel: u16,
    pub(super) planar_configuration: Option<u16>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
    pub(super) tiles_across: u32,
    pub(super) tiles_down: u32,
    pub(super) number_of_frames: u32,
    pub(super) grid: DicomGrid,
    pub(super) pixel_spacing: Option<(f64, f64)>,
    pub(super) objective_lens_power: Option<f64>,
    pub(super) encapsulated_frames: Mutex<Option<Arc<DicomEncapsulatedFrames>>>,
    pub(super) encapsulated_frame_cache: Mutex<LruCache<u32, Arc<Vec<u8>>>>,
    pub(super) decoded_frame_cache: Mutex<LruCache<u32, Arc<CpuTile>>>,
}

#[derive(Debug)]
pub(super) enum DicomGrid {
    Full,
    Sparse(HashMap<(u32, u32), u32>),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DicomFragmentRef {
    pub(super) payload_offset: u64,
    pub(super) item_offset: u64,
    pub(super) len: u32,
}

#[derive(Debug)]
pub(super) struct DicomEncapsulatedFrames {
    pub(super) fragments: Vec<DicomFragmentRef>,
    pub(super) frame_ranges: Vec<std::ops::Range<usize>>,
}

impl DicomImage {
    pub(super) fn from_metadata(meta: ParsedDicomMetadata) -> Result<Self, WsiError> {
        let width = meta.total_pixel_matrix_columns.unwrap_or(meta.columns);
        let height = meta.total_pixel_matrix_rows.unwrap_or(meta.rows);
        let tile_width = meta.columns;
        let tile_height = meta.rows;
        let tiles_across = width.div_ceil(tile_width);
        let tiles_down = height.div_ceil(tile_height);
        let grid = if meta.dimension_organization_type.as_deref() == Some("TILED_SPARSE") {
            DicomGrid::Sparse(parse_sparse_tile_map(&meta.obj, tile_width, tile_height)?)
        } else {
            DicomGrid::Full
        };
        let frame_cache_entries =
            if JP2K_TRANSFER_SYNTAXES.contains(&meta.transfer_syntax_uid.as_str()) {
                2
            } else {
                1
            };
        Ok(Self {
            path: meta.path,
            sop_instance_uid: meta.sop_instance_uid,
            transfer_syntax_uid: meta.transfer_syntax_uid,
            photometric_interpretation: meta.photometric_interpretation,
            samples_per_pixel: meta.samples_per_pixel,
            planar_configuration: meta.planar_configuration,
            width,
            height,
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
            number_of_frames: meta.number_of_frames,
            grid,
            pixel_spacing: meta.pixel_spacing,
            objective_lens_power: meta.objective_lens_power,
            encapsulated_frames: Mutex::new(None),
            encapsulated_frame_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(frame_cache_entries).unwrap(),
            )),
            decoded_frame_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(frame_cache_entries).unwrap(),
            )),
        })
    }

    pub(super) fn read_tile(
        &self,
        col: i64,
        row: i64,
        level: u32,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let span = tracing::info_span!(
            "dicom_read_tile",
            reader = "wsi_rs",
            transfer_syntax = %self.transfer_syntax_uid,
        );
        let _guard = span.enter();
        let (col_u32, row_u32) =
            checked_dicom_tile_coordinates(col, row, level, self.tiles_across, self.tiles_down)?;
        let Some(frame_index) = self.frame_index(col_u32, row_u32) else {
            let (width, height) = self.actual_tile_dimensions(col_u32, row_u32);
            return black_sample_buffer(width, height);
        };

        let (actual_width, actual_height) = self.actual_tile_dimensions(col_u32, row_u32);
        let buffer = self.decode_frame_sample_buffer(frame_index, level, col, row, backend)?;
        crop_or_keep_sample_buffer_rgb(buffer, actual_width, actual_height)
    }

    pub(super) fn read_raw_compressed_tile(
        &self,
        col: i64,
        row: i64,
        level: u32,
    ) -> Result<RawCompressedTile, WsiError> {
        let (col_u32, row_u32) =
            checked_dicom_tile_coordinates(col, row, level, self.tiles_across, self.tiles_down)?;
        let Some(frame_index) = self.frame_index(col_u32, row_u32) else {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "raw compressed tile access is not available for sparse missing DICOM tile ({col}, {row}) at level {level}"
                ),
            });
        };
        let compression = raw_compression_for_transfer_syntax(
            &self.transfer_syntax_uid,
            &self.photometric_interpretation,
        )?;
        let photometric_interpretation = raw_photometric_interpretation(
            self.samples_per_pixel,
            &self.photometric_interpretation,
        )?;
        let bytes = self.extract_encapsulated_frame(frame_index, level, col, row, true)?;
        let mut data = bytes.as_ref().clone();
        trim_encapsulated_frame_padding(&mut data);

        Ok(RawCompressedTile::builder(compression)
            .dimensions(self.tile_width, self.tile_height)
            .bits_allocated(8)
            .samples_per_pixel(self.samples_per_pixel)
            .photometric_interpretation(photometric_interpretation)
            .data(data)
            .build()?)
    }

    pub(super) fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        let buffer = self
            .decode_frame_sample_buffer(0, 0, 0, 0, BackendRequest::Auto)
            .map_err(|err| match err {
                WsiError::TileRead { reason, .. } => {
                    WsiError::AssociatedImageNotFound(format!("{name}: {reason}"))
                }
                other => other,
            })?;
        crop_or_keep_sample_buffer_rgb(buffer, self.width, self.height)
    }

    pub(super) fn frame_index(&self, col: u32, row: u32) -> Option<u32> {
        match &self.grid {
            DicomGrid::Full => Some(row * self.tiles_across + col),
            DicomGrid::Sparse(map) => map.get(&(col, row)).copied(),
        }
    }

    pub(super) fn is_full_grid(&self) -> bool {
        matches!(self.grid, DicomGrid::Full)
    }

    pub(super) fn actual_tile_dimensions(&self, col: u32, row: u32) -> (u32, u32) {
        dicom_actual_tile_dimensions(
            self.width,
            self.height,
            self.tile_width,
            self.tile_height,
            col,
            row,
        )
    }

    pub(super) fn cached_decoded_frame(&self, frame_index: u32) -> Option<Arc<CpuTile>> {
        self.decoded_frame_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&frame_index)
            .cloned()
    }

    pub(super) fn cache_decoded_frame(&self, frame_index: u32, tile: Arc<CpuTile>) {
        self.decoded_frame_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(frame_index, tile);
    }

    pub(super) fn should_cache_decoded_frames_for_batch(&self, batch_len: usize) -> bool {
        batch_len
            <= self
                .decoded_frame_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cap()
                .get()
    }

    pub(super) fn decode_uncompressed_frame_sample_buffer(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
    ) -> Result<CpuTile, WsiError> {
        let obj = reopen_dicom_object(&self.path)?;
        let pixel_data = obj
            .element(tags::PIXEL_DATA)
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!("missing pixel data: {err}"),
            })?
            .to_bytes()
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!("failed to read DICOM pixel data: {err}"),
            })?;
        let frame_len = (self.tile_width as usize)
            .checked_mul(self.tile_height as usize)
            .and_then(|pixels| pixels.checked_mul(self.samples_per_pixel as usize))
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: "DICOM frame size overflow".into(),
            })?;
        let start = (frame_index as usize)
            .checked_mul(frame_len)
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: "DICOM frame offset overflow".into(),
            })?;
        let end = start
            .checked_add(frame_len)
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: "DICOM frame byte range overflow".into(),
            })?;
        if end > pixel_data.len() {
            return Err(WsiError::TileRead {
                col,
                row,
                level,
                reason: format!(
                    "DICOM frame {frame_index} byte range {}..{} exceeds pixel data length {}",
                    start,
                    end,
                    pixel_data.len()
                ),
            });
        }
        frame_bytes_to_rgb_tile(
            &pixel_data[start..end],
            self.tile_width,
            self.tile_height,
            self.samples_per_pixel,
            self.planar_configuration.unwrap_or(0),
            &self.photometric_interpretation,
        )
        .map_err(|err| WsiError::TileRead {
            col,
            row,
            level,
            reason: err.to_string(),
        })
    }

    pub(super) fn decode_frame_sample_buffer(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let use_decoded_cache = is_encapsulated_transfer_syntax(&self.transfer_syntax_uid);
        if use_decoded_cache {
            if let Some(cached) = self.cached_decoded_frame(frame_index) {
                return Ok(cached.as_ref().clone());
            }
        }

        let buffer = if self.transfer_syntax_uid == JPEG_TRANSFER_SYNTAX {
            let bytes =
                self.extract_encapsulated_frame(frame_index, level, col, row, !use_decoded_cache)?;
            crate::core::batch::exactly_one(
                decode_batch_jpeg(&[JpegDecodeJob {
                    data: Cow::Borrowed(bytes.as_slice()),
                    tables: None,
                    expected_width: self.tile_width,
                    expected_height: self.tile_height,
                    color_transform: j2k_jpeg::ColorTransform::Auto,
                    force_dimensions: false,
                    requested_size: None,
                }]),
                "DICOM JPEG frame decode",
            )?
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: err.to_string(),
            })?
        } else if JP2K_TRANSFER_SYNTAXES.contains(&self.transfer_syntax_uid.as_str()) {
            let bytes =
                self.extract_encapsulated_frame(frame_index, level, col, row, !use_decoded_cache)?;
            crate::core::batch::exactly_one(
                decode_batch_jp2k(&[Jp2kDecodeJob {
                    data: Cow::Borrowed(bytes.as_slice()),
                    expected_width: self.tile_width,
                    expected_height: self.tile_height,
                    rgb_color_space: !jp2k_photometric_is_ycbcr(
                        self.photometric_interpretation.as_str(),
                    ),
                    backend,
                }]),
                "DICOM JP2K frame decode",
            )?
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: err.to_string(),
            })?
        } else if self.transfer_syntax_uid == RLE_TRANSFER_SYNTAX {
            let bytes =
                self.extract_encapsulated_frame(frame_index, level, col, row, !use_decoded_cache)?;
            decode_rle_lossless_frame(
                bytes.as_slice(),
                self.tile_width,
                self.tile_height,
                self.samples_per_pixel,
                &self.photometric_interpretation,
            )
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: err.to_string(),
            })?
        } else {
            self.decode_uncompressed_frame_sample_buffer(frame_index, level, col, row)?
        };

        if use_decoded_cache {
            self.cache_decoded_frame(frame_index, Arc::new(buffer.clone()));
        }
        Ok(buffer)
    }

    pub(super) fn extract_encapsulated_frame(
        &self,
        frame_index: u32,
        level: u32,
        col: i64,
        row: i64,
        cache_result: bool,
    ) -> Result<Arc<Vec<u8>>, WsiError> {
        if is_encapsulated_transfer_syntax(&self.transfer_syntax_uid) {
            if cache_result {
                if let Some(bytes) = self
                    .encapsulated_frame_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&frame_index)
                    .cloned()
                {
                    return Ok(bytes);
                }
            }
            let encapsulated_frames = self.ensure_encapsulated_frames()?;
            let frame_range = encapsulated_frames
                .frame_ranges
                .get(frame_index as usize)
                .ok_or_else(|| WsiError::TileRead {
                    col,
                    row,
                    level,
                    reason: format!(
                        "encapsulated frame {frame_index} out of range for {} frames",
                        encapsulated_frames.frame_ranges.len()
                    ),
                })?;
            let bytes = Arc::new(self.read_encapsulated_fragments(
                &encapsulated_frames.fragments[frame_range.start..frame_range.end],
            )?);
            if cache_result {
                self.encapsulated_frame_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .put(frame_index, bytes.clone());
            }
            return Ok(bytes);
        }

        let obj = reopen_dicom_object(&self.path)?;
        let pixel_data = obj
            .element(tags::PIXEL_DATA)
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!("missing pixel data: {err}"),
            })?;
        let fragments = pixel_data.fragments().ok_or_else(|| WsiError::TileRead {
            col,
            row,
            level,
            reason: "pixel data is not encapsulated".into(),
        })?;
        let number_of_frames = optional_u32(&obj, tags::NUMBER_OF_FRAMES)
            .map_err(|err| WsiError::TileRead {
                col,
                row,
                level,
                reason: err.to_string(),
            })?
            .unwrap_or(1);

        if number_of_frames == 1 && fragments.len() > 1 {
            let total_len = fragments.iter().map(Vec::len).sum();
            let mut data = Vec::with_capacity(total_len);
            for fragment in fragments {
                data.extend_from_slice(fragment);
            }
            return Ok(Arc::new(data));
        }

        fragments
            .get(frame_index as usize)
            .map(|fragment| Arc::new(fragment.as_slice().to_vec()))
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!(
                    "encapsulated frame {frame_index} out of range for {} fragments",
                    fragments.len()
                ),
            })
    }

    pub(super) fn extract_encapsulated_frames(
        &self,
        frame_indices: &[u32],
        level: u32,
        col: i64,
        row: i64,
        cache_result: bool,
    ) -> Result<HashMap<u32, Arc<Vec<u8>>>, WsiError> {
        let mut results = HashMap::with_capacity(frame_indices.len());
        if frame_indices.is_empty() {
            return Ok(results);
        }

        let mut missing = Vec::new();
        if cache_result {
            let mut cache = self
                .encapsulated_frame_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for &frame_index in frame_indices {
                if results.contains_key(&frame_index) {
                    continue;
                }
                if let Some(bytes) = cache.get(&frame_index).cloned() {
                    results.insert(frame_index, bytes);
                } else {
                    missing.push(frame_index);
                }
            }
        } else {
            for &frame_index in frame_indices {
                if !results.contains_key(&frame_index) {
                    missing.push(frame_index);
                }
            }
        }

        if missing.is_empty() {
            return Ok(results);
        }

        if !is_encapsulated_transfer_syntax(&self.transfer_syntax_uid) {
            for frame_index in missing {
                let bytes =
                    self.extract_encapsulated_frame(frame_index, level, col, row, cache_result)?;
                results.insert(frame_index, bytes);
            }
            return Ok(results);
        }

        let encapsulated_frames = self.ensure_encapsulated_frames()?;
        let mut spans = Vec::with_capacity(missing.len());
        for frame_index in missing {
            let frame_range = encapsulated_frames
                .frame_ranges
                .get(frame_index as usize)
                .ok_or_else(|| WsiError::TileRead {
                    col,
                    row,
                    level,
                    reason: format!(
                        "encapsulated frame {frame_index} out of range for {} frames",
                        encapsulated_frames.frame_ranges.len()
                    ),
                })?
                .clone();
            spans.push(self.frame_read_span(
                &encapsulated_frames,
                frame_index,
                frame_range,
                level,
                col,
                row,
            )?);
        }

        let mut file = File::open(&self.path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: self.path.clone(),
        })?;
        for group in group_frame_read_spans(spans) {
            for (frame_index, bytes) in
                self.read_encapsulated_frame_group(&mut file, &encapsulated_frames, &group)?
            {
                let bytes = Arc::new(bytes);
                if cache_result {
                    self.encapsulated_frame_cache
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .put(frame_index, bytes.clone());
                }
                results.insert(frame_index, bytes);
            }
        }

        Ok(results)
    }

    pub(super) fn ensure_encapsulated_frames(
        &self,
    ) -> Result<Arc<DicomEncapsulatedFrames>, WsiError> {
        let mut guard = self
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(frames) = &*guard {
            return Ok(frames.clone());
        }
        let frames = Arc::new(scan_encapsulated_frames(
            &self.path,
            &self.transfer_syntax_uid,
            self.number_of_frames,
        )?);
        *guard = Some(frames.clone());
        Ok(frames)
    }

    pub(super) fn read_encapsulated_fragments(
        &self,
        fragments: &[DicomFragmentRef],
    ) -> Result<Vec<u8>, WsiError> {
        let mut file = File::open(&self.path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: self.path.clone(),
        })?;
        self.read_encapsulated_fragments_with_file(&mut file, fragments)
    }

    fn read_encapsulated_fragments_with_file(
        &self,
        file: &mut File,
        fragments: &[DicomFragmentRef],
    ) -> Result<Vec<u8>, WsiError> {
        let total_len: usize = fragments.iter().map(|fragment| fragment.len as usize).sum();
        let mut data = Vec::with_capacity(total_len);
        for fragment in fragments {
            let start = data.len();
            data.resize(start + fragment.len as usize, 0);
            read_exact_at(
                file,
                &self.path,
                fragment.payload_offset,
                &mut data[start..],
            )?;
        }
        Ok(data)
    }

    fn frame_read_span(
        &self,
        encapsulated_frames: &DicomEncapsulatedFrames,
        frame_index: u32,
        frame_range: std::ops::Range<usize>,
        level: u32,
        col: i64,
        row: i64,
    ) -> Result<DicomFrameReadSpan, WsiError> {
        let fragments = encapsulated_frames
            .fragments
            .get(frame_range.clone())
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!("encapsulated frame {frame_index} has invalid fragment range"),
            })?;
        let first = fragments.first().ok_or_else(|| WsiError::TileRead {
            col,
            row,
            level,
            reason: format!("encapsulated frame {frame_index} has no fragments"),
        })?;
        let mut start = first.payload_offset;
        let mut end = first
            .payload_offset
            .checked_add(first.len as u64)
            .ok_or_else(|| WsiError::TileRead {
                col,
                row,
                level,
                reason: format!("encapsulated frame {frame_index} byte span overflow"),
            })?;
        for fragment in &fragments[1..] {
            start = start.min(fragment.payload_offset);
            let fragment_end = fragment
                .payload_offset
                .checked_add(fragment.len as u64)
                .ok_or_else(|| WsiError::TileRead {
                    col,
                    row,
                    level,
                    reason: format!("encapsulated frame {frame_index} byte span overflow"),
                })?;
            end = end.max(fragment_end);
        }
        Ok(DicomFrameReadSpan {
            frame_index,
            frame_range,
            start,
            end,
        })
    }

    fn read_encapsulated_frame_group(
        &self,
        file: &mut File,
        encapsulated_frames: &DicomEncapsulatedFrames,
        group: &DicomFrameReadGroup,
    ) -> Result<Vec<(u32, Vec<u8>)>, WsiError> {
        let span_len = group
            .end
            .checked_sub(group.start)
            .ok_or_else(|| invalid_slide(&self.path, "DICOM batch frame read span underflow"))?;
        let span_len = usize::try_from(span_len)
            .map_err(|_| invalid_slide(&self.path, "DICOM batch frame read span overflow"))?;
        let mut window = vec![0u8; span_len];
        read_exact_at(file, &self.path, group.start, &mut window)?;

        group
            .spans
            .iter()
            .map(|span| {
                let fragments = encapsulated_frames
                    .fragments
                    .get(span.frame_range.clone())
                    .ok_or_else(|| {
                        invalid_slide(&self.path, "DICOM batch frame fragment range out of bounds")
                    })?;
                let data = copy_fragments_from_window(&self.path, group.start, &window, fragments)?;
                Ok((span.frame_index, data))
            })
            .collect()
    }
}
