use crate::core::registry::composition::resolution::validate_region_request;
use crate::core::types::{Dataset, RegionRequest, Series, TileHit};
use crate::error::WsiError;

pub(super) struct RegionReadPlan<'a> {
    pub(super) series: &'a Series,
    pub(super) hits: Vec<TileHit>,
    pub(super) output_width: u32,
    pub(super) output_height: u32,
    pub(super) preserve_alpha: bool,
}

impl<'a> RegionReadPlan<'a> {
    pub(super) fn integral(
        dataset: &'a Dataset,
        request: &RegionRequest,
        max_region_pixels: u64,
    ) -> Result<Self, WsiError> {
        let (_, series, level) = validate_region_request(dataset, request)?;
        let (output_width, output_height) = request.size_px;
        check_region_pixel_limit(output_width, output_height, max_region_pixels)?;
        let hits = level.tile_layout.tiles_for_region(
            request.origin_px.0,
            request.origin_px.1,
            output_width,
            output_height,
        );
        Ok(Self {
            series,
            hits,
            output_width,
            output_height,
            preserve_alpha: false,
        })
    }

    pub(super) fn fractional(
        dataset: &'a Dataset,
        request: &RegionRequest,
        origin_px: (f64, f64),
        max_region_pixels: u64,
    ) -> Result<Self, WsiError> {
        let (_, series, level) = validate_region_request(dataset, request)?;
        let (output_width, output_height) = request.size_px;
        check_region_pixel_limit(output_width, output_height, max_region_pixels)?;
        let hits = level.tile_layout.tiles_for_fractional_region(
            origin_px.0,
            origin_px.1,
            output_width,
            output_height,
        );
        Ok(Self {
            series,
            hits,
            output_width,
            output_height,
            preserve_alpha: true,
        })
    }
}

pub(crate) fn check_region_pixel_limit(
    width: u32,
    height: u32,
    max_region_pixels: u64,
) -> Result<(), WsiError> {
    let region_pixels = u64::from(width) * u64::from(height);
    if region_pixels > max_region_pixels {
        return Err(WsiError::DisplayConversion(format!(
            "region {}x{} ({} pixels) exceeds maximum of {} pixels",
            width, height, region_pixels, max_region_pixels
        )));
    }
    Ok(())
}
