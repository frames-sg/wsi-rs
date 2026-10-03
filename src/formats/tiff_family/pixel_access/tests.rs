use super::*;
use crate::formats::tiff_family::container::TiffContainer;
use crate::formats::tiff_family::layout::DatasetLayout;
use crate::properties::Properties;
use crate::test_support::{assert_cpu_tile_matches_rgb_fixture_with_tolerance, region_request};
use flate2::write::ZlibEncoder;
use flate2::Compression as DeflateCompression;
use image::{DynamicImage, ImageFormat};

impl TiffPixelReader {
    pub(crate) fn new(container: Arc<TiffContainer>, layout: DatasetLayout) -> Self {
        Self::new_with_cache_config(container, layout, crate::CacheConfig::deterministic())
    }
}
use jpeg_encoder::{
    ColorType as JpegColorType, Encoder as JpegEncoder, SamplingFactor as JpegSamplingFactor,
};
use std::collections::HashMap;
use std::io::Cursor;
use std::io::Write;
use tempfile::NamedTempFile;

mod associated;
mod cache;
mod codecs;
#[cfg(any(feature = "metal", feature = "cuda"))]
mod device;
mod dispatch;
mod fixtures;
mod ndpi;
mod synthetic;

use fixtures::*;
