use super::region::composite_region_from_source;
use super::resolution::validate_region_request;
use std::sync::Arc;

use crate::core::cache::{CacheKey, TileCache};
use crate::core::registry::{SlideReader, DEFAULT_MAX_REGION_PIXELS};
use crate::core::types::{
    CpuTile, LevelIdx, RegionRequest, TileLayout, TileRequest, TileViewRequest,
};
use crate::error::WsiError;

pub(crate) fn read_display_tile_from_source<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &TileViewRequest,
) -> Result<CpuTile, WsiError> {
    let region_request = display_region_request(req);
    let (_, _, level) = validate_region_request(source.dataset(), &region_request)?;
    if is_exact_native_display_request(&level.tile_layout, req) {
        return read_exact_native_display_tile(source, cache, req);
    }

    compose_display_tile_fallback(source, cache, req, region_request, level.dimensions)
}

fn display_region_request(req: &TileViewRequest) -> RegionRequest {
    RegionRequest {
        scene: req.scene,
        series: req.series,
        level: LevelIdx::new(req.level.get()),
        plane: req.plane,
        origin_px: (
            req.col.saturating_mul(i64::from(req.tile_width)),
            req.row.saturating_mul(i64::from(req.tile_height)),
        ),
        size_px: (req.tile_width, req.tile_height),
    }
}

fn is_exact_native_display_request(layout: &TileLayout, req: &TileViewRequest) -> bool {
    let TileLayout::Regular {
        tile_width,
        tile_height,
        tiles_across,
        tiles_down,
    } = layout
    else {
        return false;
    };

    *tile_width == req.tile_width
        && *tile_height == req.tile_height
        && req.col >= 0
        && req.row >= 0
        && req.col < *tiles_across as i64
        && req.row < *tiles_down as i64
}

fn display_tile_request(req: &TileViewRequest) -> TileRequest {
    TileRequest {
        scene: req.scene,
        series: req.series,
        level: req.level,
        plane: req.plane,
        col: req.col,
        row: req.row,
    }
}

fn read_display_source_tile<T: SlideReader + ?Sized>(
    source: &T,
    req: &TileViewRequest,
) -> Result<CpuTile, WsiError> {
    source.read_tile_cpu(&display_tile_request(req))
}

fn read_exact_native_display_tile<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &TileViewRequest,
) -> Result<CpuTile, WsiError> {
    let started = tracing::enabled!(tracing::Level::DEBUG).then(std::time::Instant::now);
    let (tile, cache_hit) = if let Some(cache) = cache {
        let key = CacheKey::from_tile_request(source.dataset().id, &display_tile_request(req));
        if let Some(cached) = cache.get(&key) {
            (cached.as_ref().clone(), Some(true))
        } else {
            let tile = Arc::new(read_display_source_tile(source, req)?);
            cache.put(key, tile.clone());
            (tile.as_ref().clone(), Some(false))
        }
    } else {
        (read_display_source_tile(source, req)?, None)
    };
    trace_exact_native_display_tile(req, cache, cache_hit, started.as_ref());
    Ok(tile)
}

fn trace_exact_native_display_tile(
    req: &TileViewRequest,
    cache: Option<&TileCache>,
    cache_hit: Option<bool>,
    started: Option<&std::time::Instant>,
) {
    let Some(started) = started else {
        return;
    };

    match (cache, cache_hit) {
        (None, None) => tracing::debug!(
            scene = req.scene.get(),
            series = req.series.get(),
            level = req.level.get(),
            col = req.col,
            row = req.row,
            tile_width = req.tile_width,
            tile_height = req.tile_height,
            cache_enabled = false,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "wsi display tile read exact native tile"
        ),
        (Some(cache), Some(true)) => {
            let stats = cache.stats();
            tracing::debug!(
                scene = req.scene.get(),
                series = req.series.get(),
                level = req.level.get(),
                col = req.col,
                row = req.row,
                tile_width = req.tile_width,
                tile_height = req.tile_height,
                cache_hit = true,
                cache_entries = stats.entries,
                cache_current_bytes = stats.current_bytes,
                cache_capacity_bytes = stats.capacity_bytes,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                "wsi display tile read exact native tile"
            );
        }
        (Some(cache), Some(false)) => {
            let stats = cache.stats();
            tracing::debug!(
                scene = req.scene.get(),
                series = req.series.get(),
                level = req.level.get(),
                col = req.col,
                row = req.row,
                tile_width = req.tile_width,
                tile_height = req.tile_height,
                cache_hit = false,
                cache_entries = stats.entries,
                cache_current_bytes = stats.current_bytes,
                cache_capacity_bytes = stats.capacity_bytes,
                cache_evictions = stats.evictions,
                cache_rejected_oversize = stats.rejected_oversize,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                "wsi display tile read exact native tile"
            );
        }
        _ => {}
    }
}

fn compose_display_tile_fallback<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &TileViewRequest,
    region_request: RegionRequest,
    level_dimensions: (u64, u64),
) -> Result<CpuTile, WsiError> {
    let level_w = level_dimensions.0 as i64;
    let level_h = level_dimensions.1 as i64;
    if region_request.origin_px.0 >= level_w || region_request.origin_px.1 >= level_h {
        return Err(WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "display tile origin out of bounds".into(),
        });
    }

    let clipped = RegionRequest {
        size_px: (
            req.tile_width
                .min((level_w - region_request.origin_px.0) as u32),
            req.tile_height
                .min((level_h - region_request.origin_px.1) as u32),
        ),
        ..region_request
    };
    let started = tracing::enabled!(tracing::Level::DEBUG).then(std::time::Instant::now);
    let result = composite_region_from_source(source, cache, &clipped, DEFAULT_MAX_REGION_PIXELS);
    trace_display_tile_composition(req, &result, started.as_ref());
    result
}

fn trace_display_tile_composition(
    req: &TileViewRequest,
    result: &Result<CpuTile, WsiError>,
    started: Option<&std::time::Instant>,
) {
    let Some(started) = started else {
        return;
    };
    match result {
        Ok(tile) => tracing::debug!(
            scene = req.scene.get(),
            series = req.series.get(),
            level = req.level.get(),
            col = req.col,
            row = req.row,
            tile_width = req.tile_width,
            tile_height = req.tile_height,
            output_width = tile.width,
            output_height = tile.height,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "wsi display tile composed from source tiles"
        ),
        Err(err) => tracing::debug!(
            scene = req.scene.get(),
            series = req.series.get(),
            level = req.level.get(),
            col = req.col,
            row = req.row,
            tile_width = req.tile_width,
            tile_height = req.tile_height,
            error = %err,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "wsi display tile composition failed"
        ),
    }
}
