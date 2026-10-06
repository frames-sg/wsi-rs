//! Ordered logical outputs from bounded, unique MIRAX source images.
use super::*;

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
                    images.push(tile.image.as_ref());
                    images.len() - 1
                });
                plan.push((index, tile, entry.dimensions));
                source_bytes = next_source;
                live_bytes = next_live;
                largest_source = next_largest;
                output_bytes = next_output;
            }
            self.slide.probe.record_prepared_bytes(|| {
                images
                    .iter()
                    .map(|image| {
                        u64::from(image.expected_width) * u64::from(image.expected_height) * 3
                    })
                    .sum()
            });
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
                (index, self.slide.resolve_image_claim(images[index], claim))
            };
            let mut results = (0..owned.len()).map(|_| None).collect::<Vec<_>>();
            let pool = (owned.len() > 1)
                .then(crate::core::decode_runtime::process_jp2k_cpu_pool)
                .flatten();
            if let Some(pool) = pool {
                crate::core::execution_telemetry::record(
                    crate::core::execution_telemetry::Event::CpuPoolDispatches,
                    1,
                );
                // Start one source on the caller while the shared pool helps
                // with the rest; the whole read need not queue for a worker.
                pool.in_place_scope(|scope| {
                    let mut jobs = owned.into_iter().zip(results.iter_mut());
                    let first = jobs.next();
                    for (job, result) in jobs {
                        let resolve = &resolve;
                        scope.spawn(move |_| *result = Some(resolve(job)));
                    }
                    if let Some((job, result)) = first {
                        *result = Some(resolve(job));
                    }
                });
            } else {
                for (job, result) in owned.into_iter().zip(results.iter_mut()) {
                    *result = Some(resolve(job));
                }
            }
            let mut results = results
                .into_iter()
                .map(|result| result.expect("every owned source resolved"))
                .collect::<Vec<_>>();
            results.extend(waiting.into_iter().map(|(index, claim)| {
                (index, self.slide.resolve_image_claim(images[index], claim))
            }));
            results.sort_unstable_by_key(|(index, _)| *index);
            let decoded = results
                .into_iter()
                .map(|(_, tile)| tile)
                .collect::<Result<Vec<_>, _>>()?;
            start += plan.len();
            for (index, tile, dimensions) in plan {
                output.push(tile.extract(decoded[index].as_ref(), dimensions)?);
            }
        }
        Ok(output)
    }
}
