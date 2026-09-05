use std::sync::Arc;

use crate::core::cache::{CacheKey, TileCache, TileClaim};
use crate::core::registry::SlideReader;
use crate::core::types::{
    CpuTile, Dataset, Level, RegionRequest, Scene, Series, TileHit, TileRequest,
};
use crate::error::WsiError;

pub(super) fn validate_region_request<'a>(
    dataset: &'a Dataset,
    req: &RegionRequest,
) -> Result<(&'a Scene, &'a Series, &'a Level), WsiError> {
    if req.scene.get() >= dataset.scenes.len() {
        return Err(WsiError::SceneOutOfRange {
            index: req.scene.get(),
            count: dataset.scenes.len(),
        });
    }
    let scene = &dataset.scenes[req.scene.get()];

    if req.series.get() >= scene.series.len() {
        return Err(WsiError::SeriesOutOfRange {
            index: req.series.get(),
            count: scene.series.len(),
        });
    }
    let series = &scene.series[req.series.get()];

    if req.level.get() as usize >= series.levels.len() {
        return Err(WsiError::LevelOutOfRange {
            level: req.level.get(),
            count: series.levels.len() as u32,
        });
    }
    let level = &series.levels[req.level.get() as usize];

    if req.plane.get().z >= series.axes.z {
        return Err(WsiError::PlaneOutOfRange {
            axis: "z".into(),
            value: req.plane.get().z,
            max: series.axes.z,
        });
    }
    if req.plane.get().c >= series.axes.c {
        return Err(WsiError::PlaneOutOfRange {
            axis: "c".into(),
            value: req.plane.get().c,
            max: series.axes.c,
        });
    }
    if req.plane.get().t >= series.axes.t {
        return Err(WsiError::PlaneOutOfRange {
            axis: "t".into(),
            value: req.plane.get().t,
            max: series.axes.t,
        });
    }

    Ok((scene, series, level))
}

pub(super) struct RegionTileResolver<'a, T: SlideReader + ?Sized> {
    source: &'a T,
    cache: Option<&'a TileCache>,
    request: &'a RegionRequest,
}

impl<'a, T: SlideReader + ?Sized> RegionTileResolver<'a, T> {
    pub(super) fn new(
        source: &'a T,
        cache: Option<&'a TileCache>,
        request: &'a RegionRequest,
    ) -> Self {
        Self {
            source,
            cache,
            request,
        }
    }

    fn cache_key(&self, col: i64, row: i64) -> CacheKey {
        CacheKey::from_region_tile(self.source.dataset().id, self.request, col, row)
    }

    fn tile_request(&self, col: i64, row: i64) -> TileRequest {
        TileRequest {
            scene: self.request.scene.get().into(),
            series: self.request.series.get().into(),
            level: self.request.level.get().into(),
            plane: self.request.plane,
            col,
            row,
        }
    }

    pub(super) fn resolve_one(&self, col: i64, row: i64) -> Result<Arc<CpuTile>, WsiError> {
        let key = self.cache_key(col, row);
        if let Some(cached) = self.cache.and_then(|cache| cache.get(&key)) {
            return Ok(cached);
        }

        let claim = self
            .cache
            .map(|cache| cache.claim_miss(&key))
            .unwrap_or(TileClaim::Uncoalesced);
        let producer = match claim {
            TileClaim::Ready(tile) => return Ok(tile),
            TileClaim::Waiter(flight) => {
                if let Some(tile) = flight.wait() {
                    return Ok(tile);
                }
                None
            }
            TileClaim::Producer(producer) => Some(producer),
            TileClaim::Uncoalesced => None,
        };
        let tile = Arc::new(self.source.read_tile_cpu(&self.tile_request(col, row))?);
        if let Some(cache) = self.cache {
            cache.put(key, tile.clone());
        }
        if let Some(producer) = producer {
            producer.complete(tile.clone());
        }
        Ok(tile)
    }

    pub(super) fn resolve_hits(&self, hits: &[TileHit]) -> Result<Vec<Arc<CpuTile>>, WsiError> {
        let mut tiles = vec![None; hits.len()];
        let mut missed_slots = Vec::new();
        let mut missed_keys = Vec::new();
        let mut missed_reqs = Vec::new();
        let mut producers = Vec::new();
        let mut pending = Vec::new();
        let mut cache_hits = 0usize;
        let mut cache_misses = 0usize;

        for (slot, hit) in hits.iter().enumerate() {
            let key = self.cache_key(hit.col, hit.row);
            if let Some(cache) = self.cache {
                if let Some(cached) = cache.get(&key) {
                    cache_hits += 1;
                    tiles[slot] = Some(cached);
                    continue;
                }
                cache_misses += 1;
            }
            let req = self.tile_request(hit.col, hit.row);
            match self
                .cache
                .map(|cache| cache.claim_miss(&key))
                .unwrap_or(TileClaim::Uncoalesced)
            {
                TileClaim::Ready(tile) => tiles[slot] = Some(tile),
                TileClaim::Waiter(flight) => pending.push((slot, req, flight)),
                claim => {
                    producers.push(match claim {
                        TileClaim::Producer(p) => Some(p),
                        _ => None,
                    });
                    missed_slots.push(slot);
                    missed_keys.push(key);
                    missed_reqs.push(req);
                }
            }
        }

        // Publish all work owned by this batch before waiting on other batches.
        // Otherwise intersecting requests with different key orders could deadlock.
        let missed_tile_count = missed_reqs.len();
        let batched_miss_read = missed_tile_count > 1;
        if !missed_reqs.is_empty() {
            let decoded = if missed_reqs.len() == 1 {
                vec![self.source.read_tile_cpu(&missed_reqs[0])?]
            } else {
                self.source.read_tiles_cpu(&missed_reqs)?
            };
            if decoded.len() != missed_reqs.len() {
                return Err(WsiError::TileRead {
                    col: missed_reqs.first().map_or(0, |req| req.col),
                    row: missed_reqs.first().map_or(0, |req| req.row),
                    level: self.request.level.get(),
                    reason: format!(
                        "batched tile read returned {} tiles for {} requests",
                        decoded.len(),
                        missed_reqs.len()
                    ),
                });
            }

            for (((slot, key), tile), producer) in missed_slots
                .into_iter()
                .zip(missed_keys)
                .zip(decoded)
                .zip(producers)
            {
                let arc_tile = Arc::new(tile);
                if let Some(cache) = self.cache {
                    cache.put(key, arc_tile.clone());
                }
                if let Some(producer) = producer {
                    producer.complete(arc_tile.clone());
                }
                tiles[slot] = Some(arc_tile);
            }
        }

        for (slot, req, flight) in pending {
            tiles[slot] = Some(match flight.wait() {
                Some(tile) => tile,
                None => self.resolve_one(req.col, req.row)?,
            });
        }

        if let Some(cache) = self
            .cache
            .filter(|_| tracing::enabled!(tracing::Level::DEBUG))
        {
            let stats = cache.stats();
            tracing::debug!(
                requested_tiles = hits.len(),
                cache_hits,
                cache_misses,
                missed_tile_count,
                batched_miss_read,
                cache_total_hits = stats.hits,
                cache_total_misses = stats.misses,
                cache_total_puts = stats.puts,
                cache_total_evictions = stats.evictions,
                cache_rejected_oversize = stats.rejected_oversize,
                cache_entries = stats.entries,
                cache_current_bytes = stats.current_bytes,
                cache_capacity_bytes = stats.capacity_bytes,
                "wsi region tile cache resolved"
            );
        }

        tiles
            .into_iter()
            .zip(hits.iter())
            .map(|(tile, hit)| {
                tile.ok_or_else(|| WsiError::TileRead {
                    col: hit.col,
                    row: hit.row,
                    level: self.request.level.get(),
                    reason: "batched tile read did not populate requested tile".into(),
                })
            })
            .collect()
    }
}
