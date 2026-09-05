// SPDX-License-Identifier: MIT OR Apache-2.0

//! # wsi-rs
//!
//! `wsi-rs` is a whole-slide image reader focused on deterministic public
//! APIs for TIFF-family WSI, DICOM VL WSI, selected vendor containers, and
//! explicit failure behavior for unsupported inputs.
//!
//! ## Quick Start
//!
//! Read a region in level coordinates as an `image::RgbaImage`:
//!
//! ```rust,no_run
//! use wsi_rs::{LevelIdx, RegionRequest, SceneId, SeriesId, Slide};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let slide = Slide::open("sample.svs")?;
//!     let region = RegionRequest::builder(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
//!         .origin_px((0, 0))
//!         .size_px((1024, 1024))
//!         .build()?;
//!
//!     let image = slide.read_region_rgba(&region)?;
//!     image.save("region.png")?;
//!     Ok(())
//! }
//! ```
//!
//! ## Tile Reads
//!
//! Use tile-level APIs for viewers, caches, benchmarks, and workflows that need
//! exact tile coordinates:
//!
//! ```rust,no_run
//! use wsi_rs::{LevelIdx, SceneId, SeriesId, Slide, TileRequest};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let slide = Slide::open("sample.svs")?;
//!     let request = TileRequest::builder(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
//!         .tile(0, 0)
//!         .build()?;
//!
//!     let tile = slide.read_tile(&request)?;
//!     println!("{}x{} tile", tile.width(), tile.height());
//!     Ok(())
//! }
//! ```
//!
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
