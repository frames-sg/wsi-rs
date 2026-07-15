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
//! use wsi_rs::{LevelIdx, SceneId, SeriesId, Slide, TileOutputPreference, TilePixels, TileRequest};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let slide = Slide::open("sample.svs")?;
//!     let request = TileRequest::builder(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
//!         .tile(0, 0)
//!         .build()?;
//!
//!     match slide.read_tile(&request, TileOutputPreference::cpu())? {
//!         TilePixels::Cpu(tile) => {
//!             println!("{}x{} tile", tile.width(), tile.height());
//!         }
//!         TilePixels::Device(_) => unreachable!("CPU output was requested"),
//!         _ => unreachable!("CPU output was requested"),
//!     }
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
#[cfg(test)]
pub(crate) mod test_support;

pub use core::cache::CacheConfig;
pub use core::decode_runtime::{DecodeExecutionOptions, DecodeRoute, DecodeRouteDecision};
pub use error::WsiError;
pub use formats::svcache::{
    build_svcache, build_svcache_tile_payloads_merge, build_svcache_tile_payloads_replace,
    build_svcache_tiles, build_svcache_tiles_replace, cache_dir_svcache_path, default_svcache_path,
    svcache_candidate_paths, svcache_matches_source, SvcachePolicy, SvcacheTileSelection,
};
#[cfg(feature = "cuda")]
pub use output::cuda::CudaDeviceTile;
pub use properties::Properties;

#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub fn fuzz_parse_xml(input: &str) -> Result<(), WsiError> {
    decode::xml::parse_xml(input).map(drop)
}

// Multi-dimensional API
pub use core::registry::{
    DatasetReader, FormatProbe, FormatRegistry, ProbeConfidence, ProbeResult, Slide,
    SlideOpenOptions, SlideReadContext, SlideReader,
};
pub use core::types::{
    AssociatedImage, AxesShape, ChannelInfo, ColorSpace, Compression, CpuTile, CpuTileData,
    CpuTileLayout, Dataset, DatasetId, DeviceOutputContext, DeviceTile, DisplayWindow,
    EncodedTilePhotometricInterpretation, IccProfileKey, IccProfileProvenance, Level, LevelIdx,
    LevelSourceKind, OutputBackendRequest, PixelFormat, PlaneIdx, PlaneSelection,
    RawCompressedTile, RawCompressedTileBuildError, RawCompressedTileBuilder, RegionRequest,
    RegionRequestBuilder, RequestBuildError, SampleType, Scene, SceneId, Series, SeriesId,
    SourceIccProfile, SourceIccProfileConflict, SourceIccProfileKey, TileCodecKind, TileEntry,
    TileHit, TileLayout, TileOutputPreference, TilePixels, TileRequest, TileRequestBuilder,
    TileViewRequest, TileViewRequestBuilder,
};

pub mod prelude {
    //! Common imports for applications using `wsi-rs`.

    pub use crate::{
        AssociatedImage, CacheConfig, ColorSpace, CpuTile, Dataset, IccProfileKey, Level, LevelIdx,
        PixelFormat, PlaneIdx, PlaneSelection, RegionRequest, RequestBuildError, Scene, SceneId,
        Series, SeriesId, Slide, SlideOpenOptions, TileOutputPreference, TilePixels, TileRequest,
        WsiError,
    };
}
