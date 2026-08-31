use super::fractional_u8::{
    blit_fractional_saturating_u8, contract_pixman_unorm8, unpremultiply_u8,
};
use super::integral::{
    blit_integral_samples, hit_covers_output, is_integral_hit, mark_integral_tile_opaque,
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

pub(crate) fn composite_fractional_region_from_source<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    origin_px: (f64, f64),
    max_region_pixels: u64,
) -> Result<CpuTile, WsiError> {
    let plan = RegionReadPlan::fractional(source.dataset(), req, origin_px, max_region_pixels)?;
    compose_resolved_region(source, cache, req, plan)
}

pub(crate) fn composite_region_from_source_streaming<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    max_region_pixels: u64,
) -> Result<CpuTile, WsiError> {
    let plan = RegionReadPlan::integral(source.dataset(), req, max_region_pixels)?;
    compose_resolved_region_streaming(source, cache, req, plan)
}

pub(crate) fn composite_fractional_region_from_source_streaming<T: SlideReader + ?Sized>(
    source: &T,
    cache: Option<&TileCache>,
    req: &RegionRequest,
    origin_px: (f64, f64),
    max_region_pixels: u64,
) -> Result<CpuTile, WsiError> {
    let plan = RegionReadPlan::fractional(source.dataset(), req, origin_px, max_region_pixels)?;
    compose_resolved_region_streaming(source, cache, req, plan)
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

fn compose_resolved_region_streaming<T: SlideReader + ?Sized>(
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

    let first = resolver.resolve_one(plan.hits[0].col, plan.hits[0].row)?;
    if plan.hits.len() == 1
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
    for hit in &plan.hits[1..] {
        let tile = resolver.resolve_one(hit.col, hit.row)?;
        composer.blit(hit, tile.as_ref())?;
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
        let shape = CompositionShape {
            width: width as usize,
            height: height as usize,
            channels: usize::from(template.channels),
        };
        let total_samples = checked_total_samples(width, height, template.channels)?;
        let out_data = match &template.data {
            CpuTileData::U8(_) => CpuTileData::u8(vec![0u8; total_samples]),
            CpuTileData::U16(_) => CpuTileData::u16(vec![0u16; total_samples]),
            CpuTileData::F32(_) => CpuTileData::f32(vec![0.0f32; total_samples]),
        };
        let pixman_compatible = hits.iter().any(|hit| hit.cairo_fixed_dest.is_some());
        let alpha_buffer =
            if matches!(&out_data, CpuTileData::U8(_)) && hits.iter().any(needs_fractional_blit) {
                Some(vec![0.0f32; checked_region_pixels_usize(width, height)?])
            } else {
                None
            };
        Ok(Self {
            width,
            height,
            channels: template.channels,
            color_space: template.color_space.clone(),
            layout: template.layout,
            shape,
            out_data,
            alpha_buffer,
            preserve_alpha,
            pixman_compatible,
        })
    }

    fn blit(&mut self, hit: &TileHit, tile: &CpuTile) -> Result<(), WsiError> {
        tile.validate_invariants()?;
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
        if self.pixman_compatible {
            let (CpuTileData::U8(out), Some(alpha)) =
                (&mut self.out_data, self.alpha_buffer.as_deref())
            else {
                return Err(WsiError::DisplayConversion(
                    "Pixman-compatible composition requires u8 pixels and alpha state".into(),
                ));
            };
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
        if !self.preserve_alpha || !self.pixman_compatible {
            return Ok(tile);
        }

        let alpha = self
            .alpha_buffer
            .expect("Pixman-compatible composition has alpha state");
        let mut rgba = tile.into_rgba()?.into_raw();
        for (pixel, alpha) in rgba.chunks_exact_mut(4).zip(alpha) {
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

#[cfg(test)]
use super::fractional_u8::{pixman_bilinear_interpolate, unorm8_to_float};
#[cfg(test)]
use super::integral::{compose_dense_integral_u8_rows, DenseIntegralU8Hit};
#[cfg(test)]
use super::output::crop_rgb_interleaved_u8_buffer;
#[cfg(test)]
#[path = "region/tests.rs"]
mod composition_tests;
use std::sync::Arc;

use crate::core::cache::TileCache;
use crate::core::registry::SlideReader;
use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout, RegionRequest, TileHit};
use crate::error::WsiError;
