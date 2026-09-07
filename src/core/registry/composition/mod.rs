mod display;
mod fractional_u8;
mod integral;
mod output;
#[cfg(test)]
#[path = "tests/performance.rs"]
mod performance;
mod plan;
mod region;
mod resolution;

pub(crate) use display::read_display_tile_from_source;
pub(crate) use output::crop_rgb_interleaved_u8_buffer;
pub(super) use plan::RegionReadPlan;
pub(super) use region::composite_region_from_plan;
pub(crate) use region::{composite_region_from_source, composite_region_from_source_in_batches};

#[cfg(test)]
pub(super) use region::composite_fractional_region_from_source;
