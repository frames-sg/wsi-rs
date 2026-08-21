//! Shared libloading-backed OpenSlide client for parity tests.

#[allow(unused_imports)]
pub(crate) use wsi_rs_test_support::openslide::{
    parse_bounds_from_properties, try_load, OpenSlideApi as LoadedOpenSlide, OpenSlideBounds,
};
