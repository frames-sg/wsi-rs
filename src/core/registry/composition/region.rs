use super::fractional_u8::{
    blit_alpha_source_saturating_u8, blit_fractional_saturating_u8, blit_premultiplied_rgba,
    contract_pixman_unorm8, is_alpha_source, unpremultiplied_rgba_u8, unpremultiply_u8,
    RgbaBandScratch,
};
use super::integral::{
    blit_integral_rgb_saturating_rgba, blit_integral_samples, compose_dense_integral_rgb_argb32,
    has_integral_position, hit_covers_output, is_integral_hit, mark_integral_tile_opaque,
    try_compose_dense_integral_u8_region,
};
use super::output::{
    checked_region_pixels_usize, checked_total_samples, metadata_probe_coordinate,
    zero_sample_buffer_from_series, zero_sample_buffer_from_template,
};
use super::plan::RegionReadPlan;
use super::resolution::RegionTileResolver;

pub(crate) fn composite_region_from_source<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    max_region_pixels: u64,
) -> Result<CpuTile, WsiError> {
    let plan = RegionReadPlan::integral(source.dataset(), req, max_region_pixels)?;
    compose_resolved_region(source, cache, req, plan)
}

/// Compose small source units in bounded batches. The caller must fit every
/// batch's source buffers within its admitted region staging reservation.
fn plan_cache_keys<'a>(
    dataset_id: crate::core::types::DatasetId,
    req: &'a RegionRequest,
    hits: &'a [TileHit],
) -> impl Iterator<Item = CacheKey> + 'a {
    hits.iter()
        .map(move |hit| CacheKey::from_region_tile(dataset_id, req, hit.col, hit.row))
}

/// An incomplete region composes on one pool worker, and its source decodes
/// units in order there, so concurrent regions each occupy one worker rather
/// than queueing behind each other's units.
pub(crate) fn composite_region_from_source_in_batches<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    max_region_pixels: u64,
    batch_size: usize,
) -> Result<CpuTile, WsiError> {
    let plan = RegionReadPlan::integral(source.dataset(), req, max_region_pixels)?;
    let dataset_id = source.dataset().id;
    let cached = cache
        .is_some_and(|cache| cache.contains_keys(plan_cache_keys(dataset_id, req, &plan.hits)));
    let compose = || compose_resolved_region_streaming(source, cache, req, plan, batch_size.max(1));
    if cached {
        // The hint changes scheduling only. Normal resolution still handles
        // eviction and decodes a late miss on this thread.
        compose()
    } else {
        crate::core::decode_runtime::DecodeRuntime::default_arc().install_jp2k_cpu(compose)
    }
}

fn compose_resolved_region<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    plan: RegionReadPlan<'_>,
) -> Result<CpuTile, WsiError> {
    let resolver = RegionTileResolver::new(source, cache, req);

    if plan.hits.is_empty() {
        let level = &plan.series.levels[req.level.get() as usize];
        if let Some((probe_col, probe_row)) = metadata_probe_coordinate(&level.tile_layout) {
            if let Ok(template) = resolver.resolve_one(probe_col, probe_row) {
                return zero_sample_buffer_from_template(
                    plan.output_width,
                    plan.output_height,
                    template.as_ref(),
                );
            }
        }

        return zero_sample_buffer_from_series(plan.output_width, plan.output_height, plan.series);
    }

    let hit_tiles = resolver.resolve_hits(&plan.hits)?;
    compose_region_tiles(
        &plan.hits,
        &hit_tiles,
        plan.output_width,
        plan.output_height,
        plan.preserve_alpha,
    )
}

/// Writes a planned region whose tiles are all cached, opaque 8-bit RGB and
/// densely integral straight into premultiplied ARGB32. Returns `false`,
/// leaving `destination` untouched, for any other region.
pub(in crate::core::registry) fn compose_cached_region_argb32<T: SlideReader + ?Sized>(
    source: &T,
    cache: &TileCache,
    req: &RegionRequest,
    plan: &RegionReadPlan<'_>,
    destination: &mut [u32],
) -> Result<bool, WsiError> {
    // Placement is known before any cache lookup.
    if plan.hits.is_empty() || !plan.hits.iter().all(has_integral_position) {
        return Ok(false);
    }
    let Some(tiles) = cache.get_complete(plan_cache_keys(source.dataset().id, req, &plan.hits))
    else {
        return Ok(false);
    };
    for tile in &tiles {
        tile.validate_invariants()?;
    }
    compose_dense_integral_rgb_argb32(
        &plan.hits,
        &tiles,
        plan.output_width,
        plan.output_height,
        destination,
    )
}

pub(in crate::core::registry) fn composite_region_from_plan<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    plan: RegionReadPlan<'_>,
    batch_ends: &[usize],
) -> Result<CpuTile, WsiError> {
    if batch_ends.len() <= 1 {
        return compose_resolved_region(source, cache, req, plan);
    }
    if let Some(tiles) = cache
        .and_then(|cache| cache.get_complete(plan_cache_keys(source.dataset().id, req, &plan.hits)))
    {
        // Cached tiles need no decoder staging. Reuse the dense compositor
        // instead of treating an already resident region as streamed misses.
        return compose_region_tiles(
            &plan.hits,
            &tiles,
            plan.output_width,
            plan.output_height,
            plan.preserve_alpha,
        );
    }
    let resolver = RegionTileResolver::new(source, cache, req);
    let mut composer = None;
    let mut start = 0;
    for &end in batch_ends {
        let hits = &plan.hits[start..end];
        let single;
        let batch;
        let tiles: &[Arc<CpuTile>] = if hits.len() == 1 {
            single = [resolver.resolve_one(hits[0].col, hits[0].row)?];
            &single
        } else {
            batch = resolver.resolve_hits(hits)?;
            &batch
        };
        if composer.is_none() {
            composer = Some(RegionComposer::new(
                plan.output_width,
                plan.output_height,
                tiles[0].as_ref(),
                plan.preserve_alpha,
                &plan.hits,
            )?);
        }
        let composer = composer.as_mut().expect("first batch initialized composer");
        for (hit, tile) in hits.iter().zip(tiles) {
            composer.blit(hit, tile.as_ref())?;
        }
        start = end;
    }
    composer
        .expect("nonempty batch plan initialized composer")
        .finish()
}

fn compose_resolved_region_streaming<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    plan: RegionReadPlan<'_>,
    batch_size: usize,
) -> Result<CpuTile, WsiError> {
    let resolver = RegionTileResolver::new(source, cache, req);
    if plan.hits.is_empty() {
        let level = &plan.series.levels[req.level.get() as usize];
        if let Some((probe_col, probe_row)) = metadata_probe_coordinate(&level.tile_layout) {
            if let Ok(template) = resolver.resolve_one(probe_col, probe_row) {
                return zero_sample_buffer_from_template(
                    plan.output_width,
                    plan.output_height,
                    template.as_ref(),
                );
            }
        }
        return zero_sample_buffer_from_series(plan.output_width, plan.output_height, plan.series);
    }

    let first = resolver.resolve_one(plan.hits[0].col, plan.hits[0].row)?;
    if plan.hits.len() == 1
        && !composes_alpha_sources(&plan.hits, [first.as_ref()])
        && hit_covers_output(
            &plan.hits[0],
            first.as_ref(),
            plan.output_width,
            plan.output_height,
        )
    {
        return Ok(first.as_ref().clone());
    }

    let mut composer = RegionComposer::new(
        plan.output_width,
        plan.output_height,
        first.as_ref(),
        plan.preserve_alpha,
        &plan.hits,
    )?;
    composer.blit(&plan.hits[0], first.as_ref())?;
    drop(first);
    for batch in plan.hits[1..].chunks(batch_size) {
        if batch.len() == 1 {
            let tile = resolver.resolve_one(batch[0].col, batch[0].row)?;
            composer.blit(&batch[0], tile.as_ref())?;
        } else {
            let tiles = resolver.resolve_hits(batch)?;
            for (hit, tile) in batch.iter().zip(&tiles) {
                composer.blit(hit, tile.as_ref())?;
            }
        }
    }
    composer.finish()
}

fn compose_region_tiles(
    hits: &[TileHit],
    hit_tiles: &[Arc<CpuTile>],
    width: u32,
    height: u32,
    preserve_alpha: bool,
) -> Result<CpuTile, WsiError> {
    if hits.len() != hit_tiles.len() || hit_tiles.is_empty() {
        return Err(WsiError::BackendContract {
            context: "region compositor tile resolution",
            expected: hits.len(),
            actual: hit_tiles.len(),
        });
    }
    for tile in hit_tiles {
        tile.validate_invariants()?;
    }
    let first_tile = &hit_tiles[0];

    if first_tile.layout == CpuTileLayout::Planar {
        return Err(WsiError::DisplayConversion(
            "planar compositing not supported".into(),
        ));
    }
    // Coverage-carrying RGBA tiles never take the copy fast paths; the
    // composer validates each against its RGB output as it blits.
    if composes_alpha_sources(hits, hit_tiles.iter().map(Arc::as_ref)) {
        return compose_general_region(
            hits,
            hit_tiles,
            width,
            height,
            first_tile.as_ref(),
            preserve_alpha,
        );
    }
    for tile in &hit_tiles[1..] {
        if tile.data.sample_type() != first_tile.data.sample_type() {
            return Err(WsiError::DisplayConversion(
                "tile sample type mismatch during compositing".into(),
            ));
        }
        if tile.channels != first_tile.channels {
            return Err(WsiError::DisplayConversion(
                "tile channel count mismatch during compositing".into(),
            ));
        }
        if tile.color_space != first_tile.color_space {
            return Err(WsiError::DisplayConversion(
                "tile color space mismatch during compositing".into(),
            ));
        }
        if tile.layout != first_tile.layout {
            return Err(WsiError::DisplayConversion(
                "tile layout mismatch during compositing".into(),
            ));
        }
    }

    let out_channels = first_tile.channels;
    let out_color_space = first_tile.color_space.clone();
    let out_layout = first_tile.layout;
    if hits.len() == 1 && hit_covers_output(&hits[0], first_tile.as_ref(), width, height) {
        return Ok(first_tile.as_ref().clone());
    }
    if let Some(tile) = try_compose_dense_integral_u8_region(
        hits,
        hit_tiles,
        width,
        height,
        out_channels,
        &out_color_space,
        out_layout,
    )? {
        return Ok(tile);
    }

    compose_general_region(
        hits,
        hit_tiles,
        width,
        height,
        first_tile.as_ref(),
        preserve_alpha,
    )
}

fn compose_general_region(
    hits: &[TileHit],
    hit_tiles: &[Arc<CpuTile>],
    width: u32,
    height: u32,
    template: &CpuTile,
    preserve_alpha: bool,
) -> Result<CpuTile, WsiError> {
    let mut composer = RegionComposer::new(width, height, template, preserve_alpha, hits)?;
    for (hit, tile) in hits.iter().zip(hit_tiles) {
        composer.blit(hit, tile.as_ref())?;
    }
    composer.finish()
}

struct RegionComposer {
    width: u32,
    height: u32,
    channels: u16,
    color_space: ColorSpace,
    layout: CpuTileLayout,
    shape: CompositionShape,
    out_data: CpuTileData,
    alpha_buffer: Option<Vec<f32>>,
    preserve_alpha: bool,
    pixman_compatible: bool,
    direct_rgba: bool,
    rgba_needs_unpremultiply: bool,
    rgba_scratch: RgbaBandScratch,
}

impl RegionComposer {
    fn new(
        width: u32,
        height: u32,
        template: &CpuTile,
        preserve_alpha: bool,
        hits: &[TileHit],
    ) -> Result<Self, WsiError> {
        template.validate_invariants()?;
        if template.layout == CpuTileLayout::Planar {
            return Err(WsiError::DisplayConversion(
                "planar compositing not supported".into(),
            ));
        }
        let pixman_compatible = hits.iter().any(|hit| hit.cairo_fixed_dest.is_some());
        let direct_rgba = pixman_compatible
            && preserve_alpha
            && hits.iter().all(|hit| hit.cairo_fixed_dest.is_some())
            && ((template.channels == 3 && template.color_space == ColorSpace::Rgb)
                || is_alpha_source(template))
            && matches!(template.data, CpuTileData::U8(_));
        // An alpha-source template still composes RGB color plus coverage.
        let (channels, color_space) = if direct_rgba {
            (4, ColorSpace::Rgba)
        } else if pixman_compatible && is_alpha_source(template) {
            (3, ColorSpace::Rgb)
        } else {
            (template.channels, template.color_space.clone())
        };
        let shape = CompositionShape {
            width: width as usize,
            height: height as usize,
            channels: usize::from(channels),
        };
        let total_samples = checked_total_samples(width, height, channels)?;
        let out_data = match &template.data {
            CpuTileData::U8(_) => CpuTileData::u8(vec![0u8; total_samples]),
            CpuTileData::U16(_) => CpuTileData::u16(vec![0u16; total_samples]),
            CpuTileData::F32(_) => CpuTileData::f32(vec![0.0f32; total_samples]),
        };
        let alpha_buffer = if !direct_rgba
            && matches!(&out_data, CpuTileData::U8(_))
            && hits.iter().any(needs_fractional_blit)
        {
            Some(vec![0.0f32; checked_region_pixels_usize(width, height)?])
        } else {
            None
        };
        Ok(Self {
            width,
            height,
            channels,
            color_space,
            layout: template.layout,
            shape,
            out_data,
            alpha_buffer,
            preserve_alpha,
            pixman_compatible,
            direct_rgba,
            rgba_needs_unpremultiply: false,
            rgba_scratch: RgbaBandScratch::default(),
        })
    }

    fn blit(&mut self, hit: &TileHit, tile: &CpuTile) -> Result<(), WsiError> {
        tile.validate_invariants()?;
        if self.direct_rgba {
            let CpuTileData::U8(out) = &mut self.out_data else {
                unreachable!()
            };
            let out = Arc::make_mut(out);
            if !self.rgba_needs_unpremultiply
                && tile.channels == 3
                && tile.color_space == ColorSpace::Rgb
                && tile.layout == CpuTileLayout::Interleaved
            {
                let integral = hit
                    .cairo_fixed_dest
                    .filter(|(x, y)| x.fract() == 0.0 && y.fract() == 0.0);
                if let (Some(source), Some((x, y))) = (tile.as_u8(), integral) {
                    blit_integral_rgb_saturating_rgba(
                        out,
                        (self.width, self.height),
                        source,
                        tile,
                        (x as i64, y as i64),
                    );
                    return Ok(());
                }
            }
            self.rgba_needs_unpremultiply = true;
            return blit_premultiplied_rgba(out, tile, hit, self.shape, &mut self.rgba_scratch);
        }
        if self.pixman_compatible
            && self.channels == 3
            && self.color_space == ColorSpace::Rgb
            && is_alpha_source(tile)
        {
            let (CpuTileData::U8(out), Some(alpha)) =
                (&mut self.out_data, self.alpha_buffer.as_mut())
            else {
                return Err(WsiError::DisplayConversion(
                    "alpha-source composition requires u8 pixels and alpha state".into(),
                ));
            };
            let out = Arc::make_mut(out).as_mut_slice();
            return blit_alpha_source_saturating_u8(out, alpha, tile, hit, self.shape);
        }
        if tile.data.sample_type() != self.out_data.sample_type()
            || tile.channels != self.channels
            || tile.color_space != self.color_space
            || tile.layout != self.layout
        {
            return Err(WsiError::DisplayConversion(
                "tile metadata mismatch during compositing".into(),
            ));
        }
        blit_region_tile(
            &mut self.out_data,
            self.alpha_buffer.as_mut(),
            tile,
            hit,
            self.shape,
        )
    }

    fn finish(mut self) -> Result<CpuTile, WsiError> {
        if self.rgba_needs_unpremultiply {
            let CpuTileData::U8(out) = &mut self.out_data else {
                unreachable!()
            };
            super::fractional_u8::unpremultiply_rgba(Arc::make_mut(out).as_mut_slice());
        }
        if self.pixman_compatible && !self.direct_rgba {
            let (CpuTileData::U8(out), Some(alpha)) =
                (&mut self.out_data, self.alpha_buffer.as_deref())
            else {
                return Err(WsiError::DisplayConversion(
                    "Pixman-compatible composition requires u8 pixels and alpha state".into(),
                ));
            };
            if self.preserve_alpha
                && self.shape.channels == 3
                && self.layout == CpuTileLayout::Interleaved
            {
                // Unpremultiply and widen in one pass; coverage becomes alpha.
                let rgba = unpremultiplied_rgba_u8(out, alpha);
                return Ok(CpuTile {
                    width: self.width,
                    height: self.height,
                    channels: 4,
                    color_space: ColorSpace::Rgba,
                    layout: CpuTileLayout::Interleaved,
                    data: CpuTileData::u8(rgba),
                });
            }
            unpremultiply_u8(
                Arc::make_mut(out).as_mut_slice(),
                alpha,
                self.shape.channels,
            );
        }

        let tile = CpuTile {
            width: self.width,
            height: self.height,
            channels: self.channels,
            color_space: self.color_space,
            layout: self.layout,
            data: self.out_data,
        };
        if !self.preserve_alpha || !self.pixman_compatible || self.direct_rgba {
            return Ok(tile);
        }

        let alpha = self
            .alpha_buffer
            .expect("Pixman-compatible composition has alpha state");
        let mut rgba = tile.into_rgba()?.into_raw();
        for (pixel, alpha) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
            pixel[3] = contract_pixman_unorm8(alpha);
        }
        Ok(CpuTile {
            width: self.width,
            height: self.height,
            channels: 4,
            color_space: ColorSpace::Rgba,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(rgba),
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct CompositionShape {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) channels: usize,
}

fn blit_region_tile(
    out_data: &mut CpuTileData,
    alpha_buffer: Option<&mut Vec<f32>>,
    tile: &CpuTile,
    hit: &TileHit,
    shape: CompositionShape,
) -> Result<(), WsiError> {
    match (out_data, &tile.data) {
        (CpuTileData::U8(out), CpuTileData::U8(tile_data)) => {
            let out = Arc::make_mut(out);
            if needs_fractional_blit(hit) {
                let alpha = alpha_buffer.ok_or_else(|| {
                    WsiError::DisplayConversion(
                        "fractional compositing alpha buffer missing".into(),
                    )
                })?;
                blit_fractional_saturating_u8(out, alpha, tile_data.as_slice(), tile, hit, shape);
            } else {
                blit_integral_samples(out, tile_data.as_slice(), tile, hit, shape);
                if let Some(alpha) = alpha_buffer {
                    mark_integral_tile_opaque(alpha, tile, hit, shape);
                }
            }
        }
        (CpuTileData::U16(out), CpuTileData::U16(tile_data)) => blit_integral_samples(
            Arc::make_mut(out).as_mut_slice(),
            tile_data.as_slice(),
            tile,
            hit,
            shape,
        ),
        (CpuTileData::F32(out), CpuTileData::F32(tile_data)) => blit_integral_samples(
            Arc::make_mut(out).as_mut_slice(),
            tile_data.as_slice(),
            tile,
            hit,
            shape,
        ),
        _ => {
            return Err(WsiError::DisplayConversion(
                "tile sample type mismatch during compositing".into(),
            ));
        }
    }
    Ok(())
}

fn needs_fractional_blit(hit: &TileHit) -> bool {
    !is_integral_hit(hit)
}

/// Whether Pixman-compatible composition will treat any tile's straight alpha
/// as coverage, which rules out copying tiles into the output.
fn composes_alpha_sources<'a>(
    hits: &[TileHit],
    tiles: impl IntoIterator<Item = &'a CpuTile>,
) -> bool {
    hits.iter().any(|hit| hit.cairo_fixed_dest.is_some()) && tiles.into_iter().any(is_alpha_source)
}

#[cfg(test)]
#[path = "region/tests.rs"]
mod composition_tests;
use std::sync::Arc;

use crate::core::cache::{CacheKey, TileCache};
use crate::core::registry::SlideReader;
use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout, RegionRequest, TileHit};
use crate::error::WsiError;

#[cfg(test)]
#[path = "region/tests/coalescing.rs"]
mod coalescing_tests;
