use super::*;

impl TiffPixelReader {
    pub(super) fn stripped_associated_decode_pool() -> Result<&'static rayon::ThreadPool, WsiError>
    {
        static POOL: OnceLock<Result<rayon::ThreadPool, String>> = OnceLock::new();
        match POOL.get_or_init(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .use_current_thread()
                .stack_size(2 * 1024 * 1024)
                .thread_name(|idx| format!("wsi-strips-{idx}"))
                .build()
                .map_err(|err| err.to_string())
        }) {
            Ok(pool) => Ok(pool),
            Err(reason) => Err(WsiError::DisplayConversion(format!(
                "failed to build stripped associated decode pool: {reason}"
            ))),
        }
    }

    pub(super) fn read_stripped_data(
        &self,
        name: &str,
        strip_offsets: &[u64],
        strip_byte_counts: &[u64],
    ) -> Result<Vec<u8>, WsiError> {
        if strip_offsets.len() != strip_byte_counts.len() {
            return Err(WsiError::UnsupportedFormat(format!(
                "associated image '{}' has mismatched strip metadata ({} offsets vs {} byte counts)",
                name,
                strip_offsets.len(),
                strip_byte_counts.len()
            )));
        }

        let total_bytes = strip_byte_counts.iter().try_fold(0usize, |acc, count| {
            acc.checked_add(usize::try_from(*count).ok()?)
        });
        let total_bytes = total_bytes.ok_or_else(|| {
            WsiError::UnsupportedFormat(format!(
                "associated image '{}' strip byte counts exceed addressable memory",
                name
            ))
        })?;

        let mut data = Vec::with_capacity(total_bytes);
        for (&offset, &byte_count) in strip_offsets.iter().zip(strip_byte_counts.iter()) {
            if byte_count == 0 {
                continue;
            }
            let bytes = self
                .container
                .pread(offset, byte_count)
                .map_err(|e| e.into_wsi_error(self.container.path()))?;
            data.extend_from_slice(&bytes);
        }
        Ok(data)
    }

    pub(super) fn read_stripped_jpeg_image(
        &self,
        name: &str,
        ifd_id: IfdId,
        jpeg_tables: Option<&[u8]>,
        dimensions: (u32, u32),
        strip_offsets: &[u64],
        strip_byte_counts: &[u64],
    ) -> Result<CpuTile, WsiError> {
        if strip_offsets.len() != strip_byte_counts.len() {
            return Err(WsiError::UnsupportedFormat(format!(
                "associated image '{}' has mismatched strip metadata ({} offsets vs {} byte counts)",
                name,
                strip_offsets.len(),
                strip_byte_counts.len()
            )));
        }

        let (width, height) = dimensions;
        let rows_per_strip = self
            .container
            .get_u32(ifd_id, tags::ROWS_PER_STRIP)
            .unwrap_or(height)
            .max(1);
        let total_bytes = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .and_then(|px| px.checked_mul(3))
            .ok_or_else(|| {
                WsiError::UnsupportedFormat(format!(
                    "associated image '{}' dimensions overflow RGB buffer size",
                    name
                ))
            })?;
        let dst_stride = width as usize * 3;
        let strip_count = height.div_ceil(rows_per_strip) as usize;
        if strip_offsets.len() < strip_count || strip_byte_counts.len() < strip_count {
            return Err(WsiError::UnsupportedFormat(format!(
                "associated image '{}' expected at least {} strips for {} rows, found offsets={} byte_counts={}",
                name,
                strip_count,
                height,
                strip_offsets.len(),
                strip_byte_counts.len()
            )));
        }

        let mut composed = vec![0u8; total_bytes];
        let strip_chunk_bytes =
            dst_stride
                .checked_mul(rows_per_strip as usize)
                .ok_or_else(|| {
                    WsiError::UnsupportedFormat(format!(
                        "associated image '{}' rows_per_strip overflow for width {}",
                        name, width
                    ))
                })?;

        Self::stripped_associated_decode_pool()?.install(|| {
            composed
                .par_chunks_mut(strip_chunk_bytes)
                .zip(
                    strip_offsets[..strip_count]
                        .par_iter()
                        .zip(strip_byte_counts[..strip_count].par_iter())
                        .enumerate(),
                )
                .try_for_each(|(dst_chunk, (strip_idx, (&offset, &byte_count)))| {
                    if byte_count == 0 {
                        return Ok(());
                    }

                    let strip_y = rows_per_strip.saturating_mul(strip_idx as u32);
                    let strip_height = rows_per_strip.min(height - strip_y);
                    let expected_len = strip_height as usize * dst_stride;
                    if dst_chunk.len() != expected_len {
                        return Err(WsiError::UnsupportedFormat(format!(
                            "associated image '{}' destination chunk for strip {} has {} bytes, expected {}",
                            name,
                            strip_idx,
                            dst_chunk.len(),
                            expected_len
                        )));
                    }

                    let data = self
                        .container
                        .pread(offset, byte_count)
                        .map_err(|e| e.into_wsi_error(self.container.path()))?;
                    let decode_options = self.tiff_jpeg_decode_options_for_data(
                        ifd_id,
                        true,
                        &data,
                        jpeg_tables,
                    );
                    let decoded = decode_one_jpeg(
                        JpegDecodeJob {
                            data: Cow::Borrowed(&data),
                            tables: jpeg_tables.map(Cow::Borrowed),
                            expected_width: width,
                            expected_height: strip_height,
                            color_transform: decode_options.color_transform,
                            force_dimensions: decode_options.force_dimensions,
                            requested_size: None,
                        }
                    )
                    .map_err(|err| WsiError::TileRead {
                        col: strip_idx as i64,
                        row: i64::from(strip_y),
                        level: 0u32,
                        reason: format!(
                            "associated image '{}' JPEG strip {} decode failed (offset={}, bytes={}, dims={}x{}): {}",
                            name, strip_idx, offset, byte_count, width, strip_height, err
                        ),
                    })?;
                    let CpuTileData::U8(decoded_rows) = decoded.data else {
                        return Err(WsiError::DisplayConversion(
                            "stripped JPEG decode expected U8 RGB data".into(),
                        ));
                    };
                    if decoded.width != width || decoded.height != strip_height {
                        return Err(WsiError::UnsupportedFormat(format!(
                            "associated image '{}' decoded strip {} as {}x{} but expected {}x{}",
                            name, strip_idx, decoded.width, decoded.height, width, strip_height
                        )));
                    }
                    if decoded_rows.len() != expected_len {
                        return Err(WsiError::UnsupportedFormat(format!(
                            "associated image '{}' decoded strip {} produced {} bytes, expected {}",
                            name,
                            strip_idx,
                            decoded_rows.len(),
                            expected_len
                        )));
                    }

                    dst_chunk.copy_from_slice(&decoded_rows);
                    Ok(())
                })
        })?;

        Ok(CpuTile {
            width,
            height,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(composed),
        })
    }

    pub(super) fn read_tiled_associated_image(
        &self,
        name: &str,
        ifd_id: IfdId,
        jpeg_tables: Option<&[u8]>,
        compression: Compression,
        dimensions: (u32, u32),
    ) -> Result<CpuTile, WsiError> {
        #[derive(Clone, Copy)]
        struct AssociatedTileJob {
            tile_w: u32,
            tile_h: u32,
            dest_x: u32,
            dest_y: u32,
            tile_idx: usize,
        }

        let tile_width = self
            .container
            .get_u32(ifd_id, tags::TILE_WIDTH)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;
        let tile_height = self
            .container
            .get_u32(ifd_id, tags::TILE_LENGTH)
            .map_err(|e| e.into_wsi_error(self.container.path()))?;
        if tile_width == 0 || tile_height == 0 {
            return Err(WsiError::UnsupportedFormat(format!(
                "associated image '{}' has invalid tile size {}x{}",
                name, tile_width, tile_height,
            )));
        }

        let tiles_across = dimensions.0.div_ceil(tile_width);
        let tiles_down = dimensions.1.div_ceil(tile_height);
        let (offsets, byte_counts) = self.tiled_ifd_offsets_and_byte_counts(ifd_id)?;
        let required_tiles = tiles_across as usize * tiles_down as usize;
        if offsets.len() < required_tiles || byte_counts.len() < required_tiles {
            return Err(WsiError::UnsupportedFormat(format!(
                "associated image '{}' expected {} tiles, found offsets={} byte_counts={}",
                name,
                required_tiles,
                offsets.len(),
                byte_counts.len(),
            )));
        }

        let mut composed = vec![0u8; dimensions.0 as usize * dimensions.1 as usize * 3];
        let composed_stride = dimensions.0 as usize * 3;
        let mut tile_jobs = Vec::with_capacity(tiles_across as usize * tiles_down as usize);
        for row in 0..tiles_down {
            for col in 0..tiles_across {
                tile_jobs.push(AssociatedTileJob {
                    tile_w: tile_width.min(dimensions.0.saturating_sub(col * tile_width)),
                    tile_h: tile_height.min(dimensions.1.saturating_sub(row * tile_height)),
                    dest_x: col * tile_width,
                    dest_y: row * tile_height,
                    tile_idx: (row * tiles_across + col) as usize,
                });
            }
        }

        let decoded_tiles: Vec<_> = if tile_jobs.len() <= 1 {
            tile_jobs
                .into_iter()
                .map(|job| {
                    let tile = self.decode_tiled_ifd_tile_index(
                        ifd_id,
                        job.tile_idx,
                        jpeg_tables,
                        compression,
                        job.tile_w,
                        job.tile_h,
                        offsets,
                        byte_counts,
                        BackendRequest::Auto,
                    )?;
                    Ok((job, tile))
                })
                .collect::<Result<_, WsiError>>()?
        } else {
            tile_jobs
                .into_par_iter()
                .map(|job| {
                    let tile = self.decode_tiled_ifd_tile_index(
                        ifd_id,
                        job.tile_idx,
                        jpeg_tables,
                        compression,
                        job.tile_w,
                        job.tile_h,
                        offsets,
                        byte_counts,
                        BackendRequest::Auto,
                    )?;
                    Ok((job, tile))
                })
                .collect::<Result<_, WsiError>>()?
        };

        for (job, tile) in decoded_tiles {
            match (&tile.data, tile.layout, tile.channels, &tile.color_space) {
                (CpuTileData::U8(tile_rgb), CpuTileLayout::Interleaved, 3, ColorSpace::Rgb) => {
                    let tile_src_stride = job.tile_w as usize * 3;
                    for y in 0..job.tile_h as usize {
                        let src_row = y * tile_src_stride;
                        let dst_row =
                            (job.dest_y as usize + y) * composed_stride + job.dest_x as usize * 3;
                        composed[dst_row..dst_row + tile_src_stride]
                            .copy_from_slice(&tile_rgb[src_row..src_row + tile_src_stride]);
                    }
                }
                _ => {
                    let tile_rgba = tile.to_rgba()?;
                    let tile_rgba_raw = tile_rgba.as_raw();
                    let tile_src_stride = job.tile_w as usize * 4;
                    for y in 0..job.tile_h as usize {
                        let src_row = y * tile_src_stride;
                        let dst_row =
                            (job.dest_y as usize + y) * composed_stride + job.dest_x as usize * 3;
                        let src_pixels = &tile_rgba_raw[src_row..src_row + tile_src_stride];
                        let dst_pixels = &mut composed[dst_row..dst_row + job.tile_w as usize * 3];
                        for (src_px, dst_px) in src_pixels
                            .chunks_exact(4)
                            .zip(dst_pixels.chunks_exact_mut(3))
                        {
                            dst_px.copy_from_slice(&src_px[..3]);
                        }
                    }
                }
            }
        }

        Ok(CpuTile {
            width: dimensions.0,
            height: dimensions.1,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(composed),
        })
    }
}
