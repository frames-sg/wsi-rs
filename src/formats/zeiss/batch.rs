//! Operation-local CZI source blocks shared by ordered logical tile outputs.
use super::*;
use rayon::prelude::*;

pub(super) enum PreparedSubblock {
    Raw(czi_rs::RawSubBlock),
    Decoded(Arc<CpuTile>),
}
pub(super) type PreparedSubblocks = HashMap<u64, PreparedSubblock>;

impl ZeissReader {
    pub(super) fn read_cpu_batch(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        if reqs.len() <= 1 {
            return reqs.iter().map(|req| self.read_tile_cpu(req)).collect();
        }
        if let Some(tiles) = self.cached_batch(reqs) {
            return Ok(tiles);
        }
        let runtime = crate::core::decode_runtime::DecodeRuntime::default_arc();
        let limits = self.slide.limits;
        let logical_bytes = reqs
            .iter()
            .filter_map(|req| self.batch_tile_geometry(req))
            .fold(0_u64, |sum, (_, _, width, height, _)| {
                sum.saturating_add(
                    u64::from(width)
                        .saturating_mul(u64::from(height))
                        .saturating_mul(4),
                )
            });
        // Stay inside the enclosing reader's existing encoded-unit + two
        // output-buffer reservation, in addition to all configured ceilings.
        let target = limits
            .batch_chunk_bytes()
            .min(limits.operation_transient_bytes())
            .min(limits.slide_transient_bytes())
            .min(
                limits
                    .encoded_unit_bytes()
                    .saturating_add(logical_bytes.saturating_mul(2)),
            );
        let workers = runtime.cpu_worker_count();
        let mut output = Vec::with_capacity(reqs.len());
        let mut start = 0;
        while start < reqs.len() {
            let mut sources = HashMap::new();
            let mut order = Vec::new();
            let mut cached = Vec::new();
            let mut work = 0_u64;
            let mut live_bytes = 0_u64;
            let mut largest_source = 0_u64;
            for req in &reqs[start..] {
                let Some((x, y, width, height, ratio)) = self.batch_tile_geometry(req) else {
                    break;
                };
                let tile = self
                    .slide
                    .tile_cache
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(&(req.scene.get(), req.level.get() as usize, req.col, req.row))
                    .cloned();
                if let Some(tile) = tile {
                    cached.push(Some(tile));
                    continue;
                }
                let Some(infos) = self.batch_source_infos(req, (x, y, width, height), ratio)?
                else {
                    break;
                };
                let mut additions: Vec<czi_rs::DirectorySubBlockInfo> = Vec::new();
                let mut next_live = live_bytes;
                let mut next_largest = largest_source;
                let mut next_work = work.saturating_add(u64::from(width) * u64::from(height) * 3);
                for info in infos {
                    if sources.contains_key(&info.file_position)
                        || additions
                            .iter()
                            .any(|prior| prior.file_position == info.file_position)
                    {
                        continue;
                    }
                    let encoded = self
                        .slide
                        .source_subblock_encoded_upper_bound(info.file_position)?;
                    let source_pixels = u64::from(info.stored_size.w)
                        .saturating_mul(u64::from(info.stored_size.h))
                        .saturating_mul(info.pixel_type.bytes_per_pixel() as u64);
                    let (retained, codec_work) =
                        if info.compression == CziCompressionMode::UnCompressed {
                            (encoded, 0)
                        } else {
                            (source_pixels, source_pixels.saturating_mul(2))
                        };
                    next_live = next_live.saturating_add(retained);
                    next_largest = next_largest.max(retained);
                    next_work = next_work.saturating_add(encoded).saturating_add(codec_work);
                    additions.push(info);
                }
                // Preserve the existing decoded staging scale. A large source
                // streams alone; spare encoded allowance does not expand the
                // number of decoded blocks retained at the same time.
                if next_work > target
                    || sources.len() + additions.len() > workers
                    || next_live > logical_bytes.max(next_largest)
                {
                    break;
                }
                for info in additions {
                    order.push(info.file_position);
                    sources.insert(info.file_position, info);
                }
                work = next_work;
                live_bytes = next_live;
                largest_source = next_largest;
                cached.push(None);
            }
            if cached.is_empty() {
                output.push(self.read_tile_cpu(&reqs[start])?);
                start += 1;
                continue;
            }
            // Source ownership precedes pool dispatch. Wait only after all
            // local producers have completed, on the original calling thread.
            #[cfg(test)]
            self.slide.prepared_source_peak_bytes.fetch_max(
                sources
                    .values()
                    .map(|info| {
                        u64::from(info.stored_size.w)
                            * u64::from(info.stored_size.h)
                            * info.pixel_type.bytes_per_pixel() as u64
                    })
                    .sum(),
                Ordering::Relaxed,
            );
            let claims: Vec<_> = order
                .iter()
                .enumerate()
                .map(|(index, key)| {
                    let info = &sources[key];
                    let claim = (info.compression != CziCompressionMode::UnCompressed)
                        .then(|| self.slide.claim_subblock(info));
                    (index, *key, claim)
                })
                .collect();
            let (waiting, owned): (Vec<_>, Vec<_>) =
                claims.into_iter().partition(|(_, _, claim)| {
                    matches!(claim, Some(crate::core::cache::TileClaim::Waiter(_)))
                });
            let decode = |(index, key, claim)| {
                let info = &sources[&key];
                let result = match claim {
                    Some(claim) => self
                        .slide
                        .resolve_subblock_claim(info, claim)
                        .map(PreparedSubblock::Decoded),
                    None => self
                        .slide
                        .read_source_subblock(info)
                        .map(PreparedSubblock::Raw),
                };
                (index, key, result)
            };
            let mut results = if owned
                .iter()
                .filter(|(_, _, claim)| {
                    !matches!(claim, Some(crate::core::cache::TileClaim::Ready(_)))
                })
                .count()
                <= 1
            {
                owned.into_iter().map(decode).collect::<Vec<_>>()
            } else {
                runtime.install_jp2k_cpu(|| owned.into_par_iter().map(decode).collect::<Vec<_>>())
            };
            results.extend(waiting.into_iter().map(decode));
            results.sort_unstable_by_key(|(index, _, _)| *index);
            let prepared = results
                .into_iter()
                .map(|(_, key, result)| result.map(|tile| (key, tile)))
                .collect::<Result<PreparedSubblocks, _>>()?;
            let end = start + cached.len();
            for (req, cached) in reqs[start..end].iter().zip(cached) {
                output.push(match cached {
                    Some(tile) => tile,
                    None => self.slide.read_tile_with_sources(
                        req.scene.get(),
                        req.series.get(),
                        req.level.get(),
                        req.col,
                        req.row,
                        Some(&prepared),
                    )?,
                });
            }
            start = end;
        }
        Ok(output)
    }

    fn cached_batch(&self, reqs: &[TileRequest]) -> Option<Vec<CpuTile>> {
        let mut cache = self
            .slide
            .tile_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        reqs.iter()
            .map(|req| {
                self.batch_tile_geometry(req)?;
                cache
                    .get(&(req.scene.get(), req.level.get() as usize, req.col, req.row))
                    .cloned()
            })
            .collect()
    }

    fn batch_tile_geometry(&self, req: &TileRequest) -> Option<(i64, i64, u32, u32, i64)> {
        let level = self
            .slide
            .dataset
            .scenes
            .get(req.scene.get())?
            .series
            .get(req.series.get())?
            .levels
            .get(req.level.get() as usize)?;
        let TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } = level.tile_layout
        else {
            return None;
        };
        if req.col < 0
            || req.row < 0
            || req.col as u64 >= tiles_across
            || req.row as u64 >= tiles_down
        {
            return None;
        }
        let x = req.col.checked_mul(i64::from(tile_width))?;
        let y = req.row.checked_mul(i64::from(tile_height))?;
        let width = level
            .dimensions
            .0
            .saturating_sub(x as u64)
            .min(u64::from(tile_width)) as u32;
        let height = level
            .dimensions
            .1
            .saturating_sub(y as u64)
            .min(u64::from(tile_height)) as u32;
        Some((
            x,
            y,
            width,
            height,
            level.downsample.round().max(1.0) as i64,
        ))
    }

    fn batch_source_infos(
        &self,
        req: &TileRequest,
        (x, y, width, height): (i64, i64, u32, u32),
        ratio: i64,
    ) -> Result<Option<Vec<czi_rs::DirectorySubBlockInfo>>, WsiError> {
        let indices = self
            .slide
            .canvas_level_tile_subblocks
            .get(req.level.get() as usize)
            .and_then(|tiles| tiles.get(&(req.col, req.row)));
        let mut infos = Vec::new();
        let czi = self
            .slide
            .czi
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for &index in indices.into_iter().flatten() {
            let info = czi.subblocks().get(index).ok_or_else(|| {
                WsiError::DisplayConversion(format!("Zeiss subblock index {index} out of range"))
            })?;
            if !matches!(
                info.compression,
                CziCompressionMode::UnCompressed
                    | CziCompressionMode::Jpg
                    | CziCompressionMode::JpgXr
            ) || !matches!(info.pixel_type, CziPixelType::Bgr24 | CziPixelType::Bgra32)
            {
                return Ok(None);
            }
            let sx = (i64::from(info.rect.x) - i64::from(self.slide.subblock_origin.0))
                .div_euclid(ratio);
            let sy = (i64::from(info.rect.y) - i64::from(self.slide.subblock_origin.1))
                .div_euclid(ratio);
            if sx < x + i64::from(width)
                && sy < y + i64::from(height)
                && sx + i64::from(info.stored_size.w) > x
                && sy + i64::from(info.stored_size.h) > y
            {
                infos.push(info.clone());
            }
        }
        Ok(Some(infos))
    }
}
