//! Shared test-support helpers for the wsi_rs parity harness.

#![allow(dead_code)]

use wsi_rs::{LevelIdx, PlaneIdx, PlaneSelection, RegionRequest, SceneId, SeriesId};

pub(crate) mod compare;
pub(crate) mod corpus;
pub(crate) mod oracles;

#[cfg(feature = "parity-openslide")]
pub(crate) mod openslide_shim;

#[allow(clippy::too_many_arguments)]
pub(crate) fn region_request(
    scene: usize,
    series: usize,
    level: u32,
    plane: PlaneSelection,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> RegionRequest {
    RegionRequest::new(
        SceneId::new(scene),
        SeriesId::new(series),
        LevelIdx::new(level),
        (x, y),
        (w, h),
    )
    .with_plane(PlaneIdx::new(plane))
}
