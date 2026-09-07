use super::*;
use crate::core::registry::composition::RegionReadPlan;

pub(super) struct PlannedRegionRead<'a> {
    pub(super) plan: RegionReadPlan<'a>,
    pub(super) work_bytes: u64,
    sizes: Vec<(u64, u64)>,
    encoded: u64,
}

impl Slide {
    pub(super) fn plan_region_read<'a>(
        &'a self,
        req: &RegionRequest,
        origin: Option<(f64, f64)>,
        output_bytes: u64,
    ) -> Result<PlannedRegionRead<'a>, WsiError> {
        let plan = match origin {
            Some(origin) => RegionReadPlan::fractional(
                self.dataset(),
                req,
                origin,
                self.limits.region_pixels(),
            )?,
            None => RegionReadPlan::integral(self.dataset(), req, self.limits.region_pixels())?,
        };
        let encoded = self.source.region_fastpath_encoded_upper_bound(req)?;
        let sizes = plan
            .hits
            .iter()
            .map(|hit| {
                let tile = TileRequest {
                    scene: req.scene,
                    series: req.series,
                    level: req.level,
                    plane: req.plane,
                    col: hit.col,
                    row: hit.row,
                };
                Ok((self.estimate_tile_output_bytes(&tile)?, 0))
            })
            .collect::<Result<Vec<_>, WsiError>>()?;
        let largest = sizes.iter().map(|&(decoded, _)| decoded).max().unwrap_or(0);
        let work = self.region_work_bytes(encoded, output_bytes, largest, origin.is_some())?;
        Ok(PlannedRegionRead {
            plan,
            work_bytes: work,
            sizes,
            encoded,
        })
    }
}

impl PlannedRegionRead<'_> {
    pub(super) fn batch_ends(
        &mut self,
        slide: &Slide,
        req: &RegionRequest,
        output_bytes: u64,
    ) -> Result<Vec<usize>, WsiError> {
        // A handled format fast path needs only its own encoded bound. Query
        // generic tile inputs only when generic composition will execute them.
        for ((_, encoded), hit) in self.sizes.iter_mut().zip(&self.plan.hits) {
            *encoded = slide.source.tile_encoded_upper_bound(&TileRequest {
                scene: req.scene,
                series: req.series,
                level: req.level,
                plane: req.plane,
                col: hit.col,
                row: hit.row,
            })?;
        }
        let sizes = &self.sizes;
        let encoded = self.encoded;
        let largest = sizes.iter().map(|&(decoded, _)| decoded).max().unwrap_or(0);
        // Wider streamed batches raised measured RSS by 15–22%. Keep decoded
        // and codec staging below half the output allowance and 1 MiB. Small
        // concurrent regions otherwise still exceeded the RSS ceiling. A larger
        // individual source and complete batches retain their existing paths.
        let staging = (output_bytes / 2).min(1024 * 1024).max(largest);
        let (all_decoded, all_encoded) = sizes.iter().fold(
            (0_u64, 0_u64),
            |(decoded, encoded), &(next_decoded, next_encoded)| {
                (
                    decoded.saturating_add(next_decoded),
                    encoded.saturating_add(next_encoded),
                )
            },
        );
        // Before composition starts, both output-sized allowances are available
        // for a complete tile batch. Preserve this existing dense path even with
        // one worker: the codec limits concurrency, not the result cardinality.
        if all_decoded <= output_bytes && all_encoded <= encoded {
            let ends = if sizes.is_empty() {
                Vec::new()
            } else {
                vec![sizes.len()]
            };
            return Ok(ends);
        }
        let workers = slide.decode_runtime.cpu_worker_count();
        let mut batch_ends = Vec::new();
        let mut start = 0;
        while start < sizes.len() {
            let mut end = start + 1;
            let (mut decoded, mut inputs) = sizes[start];
            while end < sizes.len() && end - start < workers {
                let next_decoded = decoded.saturating_add(sizes[end].0);
                let next_inputs = inputs.saturating_add(sizes[end].1);
                // Streaming already retains the composed output. Both decoded
                // tiles and codec work must fit its remaining staging allowance;
                // unused encoded capacity never expands decoded concurrency.
                if next_decoded.saturating_mul(2) > staging || next_inputs > encoded {
                    break;
                }
                decoded = next_decoded;
                inputs = next_inputs;
                end += 1;
            }
            batch_ends.push(end);
            start = end;
        }
        Ok(batch_ends)
    }
}
