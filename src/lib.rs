// SPDX-License-Identifier: MIT OR Apache-2.0

//! # wsi-rs
//!
//! Read whole-slide images: microscope slide scans stored as tiled,
//! multi-resolution pyramids in scanner-specific file formats.
//!
//! Open a file with [`Slide::open`]. [`Slide::dataset`] describes what is
//! inside: scenes, each with a pyramid of levels (level 0 is full resolution),
//! associated images such as the slide label, and scanner metadata. Read
//! pixels with [`Slide::read_region_rgba`] for any rectangle, or
//! [`Slide::read_tile`] for the tiles exactly as the file stores them. The API
//! is the same for every supported format.
//!
//! Malformed files, unsupported data and requests over the configured
//! [`SlideLimits`] return a [`WsiError`]. A read never returns black or
//! partial pixels in place of an error.
//!
//! ## Read a region
//!
//! ```rust,no_run
//! use wsi_rs::{RegionRequest, Slide};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let slide = Slide::open("sample.svs")?;
//!
//!     // The pyramid of the first image, from full resolution down.
//!     let levels = &slide.dataset().scenes[0].series[0].levels;
//!     for (index, level) in levels.iter().enumerate() {
//!         println!(
//!             "level {index}: {} x {} pixels, downsample {}",
//!             level.dimensions.0, level.dimensions.1, level.downsample
//!         );
//!     }
//!
//!     // A 1024 x 1024 region from the top-left corner of level 0.
//!     let region = RegionRequest::builder(0usize, 0usize, 0u32)
//!         .origin_px((0, 0))
//!         .size_px((1024, 1024))
//!         .build()?;
//!     slide.read_region_rgba(&region)?.save("region.png")?;
//!
//!     // The photo of the slide label, if the scanner stored one.
//!     if slide.dataset().associated_images.contains_key("label") {
//!         slide.read_associated("label")?.to_rgba()?.save("label.png")?;
//!     }
//!
//!     // Scanner metadata, using OpenSlide's property names.
//!     if let Some(mpp) = slide.dataset().properties.get("openslide.mpp-x") {
//!         println!("{mpp} microns per pixel");
//!     }
//!     Ok(())
//! }
//! ```
//!
//! ## Read tiles
//!
//! Viewers and tile servers read tiles directly. [`Slide::read_tile`] returns a
//! tile as the file stores it; [`Slide::read_display_tile`] returns tiles on a
//! regular grid of a size you choose.
//!
//! ```rust,no_run
//! use wsi_rs::{Slide, TileRequest};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let slide = Slide::open("sample.svs")?;
//!     let request = TileRequest::builder(0usize, 0usize, 0u32).tile(0, 0).build()?;
//!     let tile = slide.read_tile(&request)?;
//!     println!("{} x {} tile", tile.width(), tile.height());
//!     Ok(())
//! }
//! ```
//!
//! ## Options
//!
//! [`Slide::open`] uses default cache sizes and limits. Use
//! [`Slide::open_with_options`] and [`SlideOpenOptions`] to change cache sizes,
//! resource limits, CPU or GPU decoding, and `.svcache` lookup.
//!
//! ```rust,no_run
//! use wsi_rs::{DecodeAcceleration, DecodeExecutionOptions, Slide, SlideOpenOptions};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let options = SlideOpenOptions::default().with_decode_execution_options(
//!         DecodeExecutionOptions::default().with_acceleration(DecodeAcceleration::CpuOnly),
//!     );
//!     let slide = Slide::open_with_options("sample.svs", options)?;
//!     println!("{} scenes", slide.dataset().scenes.len());
//!     Ok(())
//! }
//! ```
#![deny(unsafe_code)]

pub(crate) mod core;
pub(crate) mod decode;
pub mod error;
pub(crate) mod formats;
pub mod output;
pub mod properties;
mod slide_candidates;
#[cfg(test)]
pub(crate) mod test_support;

pub use core::cache::{CacheConfig, TileCache, TileCacheStats};
#[cfg(feature = "route-telemetry")]
#[doc(hidden)]
pub use core::decode_runtime::decode_route_telemetry_json;
pub use core::decode_runtime::{DecodeAcceleration, DecodeExecutionOptions};
pub use core::read_control::{
    DicomIndexDiagnostic, DicomIndexMapping, DicomIndexOutcome, ReadCancellationToken, ReadControl,
    ReadDiagnosticSink,
};
pub use error::WsiError;
pub use formats::svcache::{
    build_svcache, build_svcache_tile_payloads_merge, build_svcache_tile_payloads_replace,
    build_svcache_tiles, build_svcache_tiles_replace, cache_dir_svcache_path, default_svcache_path,
    svcache_candidate_paths, svcache_matches_source, SvcachePolicy, SvcacheTileSelection,
};
pub use properties::Properties;
pub use slide_candidates::{is_builtin_slide_candidate_path, BUILTIN_SLIDE_CANDIDATE_EXTENSIONS};

#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub fn fuzz_parse_xml(input: &str) -> Result<(), WsiError> {
    decode::xml::parse_xml(input).map(drop)
}

// Multi-dimensional API
pub use core::registry::{
    DatasetReader, FormatProbe, FormatRegistry, ProbeConfidence, ProbeResult, Slide,
    SlideLimitError, SlideLimits, SlideOpenOptions, SlideReadContext, SlideReader,
};
pub use core::types::{
    AssociatedImage, AxesShape, ChannelInfo, ColorSpace, Compression, CpuTile, CpuTileData,
    CpuTileLayout, Dataset, DatasetId, DisplayWindow, EncodedTilePhotometricInterpretation,
    IccProfileKey, IccProfileProvenance, Level, LevelIdx, LevelSourceKind, PixelFormat, PlaneIdx,
    PlaneSelection, RawCompressedTile, RawCompressedTileBuildError, RawCompressedTileBuilder,
    RegionRequest, RegionRequestBuilder, RequestBuildError, SampleType, Scene, SceneId, Series,
    SeriesId, SourceIccProfile, SourceIccProfileConflict, SourceIccProfileKey, TileCodecKind,
    TileEntry, TileHit, TileLayout, TileRequest, TileRequestBuilder, TileViewRequest,
    TileViewRequestBuilder,
};

pub mod prelude {
    //! Common imports for applications using `wsi-rs`.

    pub use crate::{
        AssociatedImage, CacheConfig, ColorSpace, CpuTile, Dataset, IccProfileKey, Level, LevelIdx,
        PixelFormat, PlaneIdx, PlaneSelection, RegionRequest, RequestBuildError, Scene, SceneId,
        Series, SeriesId, Slide, SlideLimits, SlideOpenOptions, TileRequest, WsiError,
    };
}
