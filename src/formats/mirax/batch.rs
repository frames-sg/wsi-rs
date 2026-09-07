//! Ordered logical outputs from bounded, unique MIRAX source images.
use super::*;
use rayon::prelude::*;

impl MiraxReader {
    pub(super) fn read_cpu_batch(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        if reqs.len() <= 1 {
            return reqs
                .iter()
                .map(|req| self.read_tile_with_backend(req, BackendRequest::Cpu))
                .collect();
        }
        let runtime = crate::core::decode_runtime::DecodeRuntime::default_arc();
        let workers = runtime.cpu_worker_count();
        let limits = self.slide.limits;
        let logical_bytes = reqs.iter().try_fold(0_u64, |sum, req| {
            let (entry, _) = self.tile_for_request(req)?;
            Ok::<_, WsiError>(
                sum.saturating_add(
                    u64::from(entry.dimensions.0)
                        .saturating_mul(u64::from(entry.dimensions.1))
                        .saturating_mul(4),
                ),
            )
        })?;
        let target = limits
            .batch_chunk_bytes()
            .min(limits.operation_transient_bytes())
            .min(limits.slide_transient_bytes())
            .min(
                limits
                    .encoded_unit_bytes()
                    .saturating_add(logical_bytes.saturating_mul(2)),
            );
        let mut output = Vec::with_capacity(reqs.len());
        let mut start = 0;
        while start < reqs.len() {
            let mut indices = HashMap::new();
            let mut images = Vec::new();
            let mut plan = Vec::new();
            let mut source_bytes = 0_u64;
            let mut live_bytes = 0_u64;
            let mut largest_source = 0_u64;
            let mut output_bytes = 0_u64;
            for req in &reqs[start..] {
                let (entry, tile) = self.tile_for_request(req)?;
                let next_output = output_bytes.saturating_add(
                    u64::from(entry.dimensions.0)
                        .saturating_mul(u64::from(entry.dimensions.1))
                        .saturating_mul(3),
                );
                let new_image = !indices.contains_key(&tile.image.id);
                let pixels = u64::from(tile.image.expected_width)
                    .saturating_mul(u64::from(tile.image.expected_height))
                    .saturating_mul(3);
                let next_live = live_bytes.saturating_add(if new_image { pixels } else { 0 });
                let next_largest = largest_source.max(pixels);
                let next_source = source_bytes.saturating_add(if new_image {
                    u64::from(tile.image.expected_width)
                        .saturating_mul(u64::from(tile.image.expected_height))
                        .saturating_mul(6)
                        .saturating_add(tile.image.record.len)
                } else {
                    0
                });
                if !plan.is_empty()
                    && (next_source.saturating_add(next_output) > target
                        || next_live > logical_bytes.max(next_largest)
                        || (new_image && images.len() >= workers))
                {
                    break;
                }
                let index = *indices.entry(tile.image.id).or_insert_with(|| {
                    images.push(tile.image.clone());
                    images.len() - 1
                });
                plan.push((index, tile.src_x, tile.src_y, entry.dimensions));
                source_bytes = next_source;
                live_bytes = next_live;
                largest_source = next_largest;
                output_bytes = next_output;
            }
            #[cfg(test)]
            self.slide.prepared_source_peak_bytes.fetch_max(
                images
                    .iter()
                    .map(|image| {
                        u64::from(image.expected_width) * u64::from(image.expected_height) * 3
                    })
                    .sum(),
                Ordering::Relaxed,
            );
            // Claim on the calling thread. Publish every owned source before
            // waiting for other batches, so reversed overlaps cannot deadlock.
            let claims: Vec<_> = images
                .iter()
                .enumerate()
                .map(|(index, image)| (index, self.slide.claim_image(image)))
                .collect();
            let (waiting, owned): (Vec<_>, Vec<_>) = claims
                .into_iter()
                .partition(|(_, claim)| matches!(claim, crate::core::cache::TileClaim::Waiter(_)));
            let resolve = |(index, claim): (usize, crate::core::cache::TileClaim<'_, u32>)| {
                (
                    index,
                    self.slide
                        .resolve_image_claim(images[index].as_ref(), claim),
                )
            };
            let mut results = if owned
                .iter()
                .filter(|(_, claim)| !matches!(claim, crate::core::cache::TileClaim::Ready(_)))
                .count()
                <= 1
            {
                owned.into_iter().map(resolve).collect::<Vec<_>>()
            } else {
                runtime.install_jp2k_cpu(|| owned.into_par_iter().map(resolve).collect::<Vec<_>>())
            };
            results.extend(waiting.into_iter().map(|(index, claim)| {
                (index, self.slide.resolve_image_claim(&images[index], claim))
            }));
            results.sort_unstable_by_key(|(index, _)| *index);
            let decoded = results
                .into_iter()
                .map(|(_, tile)| tile)
                .collect::<Result<Vec<_>, _>>()?;
            start += plan.len();
            for (index, x, y, (width, height)) in plan {
                let source = decoded[index].as_ref();
                output.push(
                    if x == 0 && y == 0 && (source.width, source.height) == (width, height) {
                        source.clone()
                    } else {
                        crop_rgb_interleaved_u8_buffer(source, x, y, width, height)?
                    },
                );
            }
        }
        Ok(output)
    }
}
