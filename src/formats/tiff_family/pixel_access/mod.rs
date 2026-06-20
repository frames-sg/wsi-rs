//! Layer 3: Pixel access — TiffPixelReader and decode helpers.
//!
//! TiffPixelReader implements SlideReader by dispatching tile reads to focused
//! helper modules based on the `TileSource` variant.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use lru::LruCache;
use rayon::prelude::*;
use signinum_core::BackendRequest;
use signinum_jpeg::transcode::{
    extract_dct_blocks, DctExtractOptions, JpegDctCodingMode, JpegDctComponent, JpegDctImage,
};
use signinum_jpeg::{
    ColorTransform as SigninumColorTransform, DecodeOptions as SigninumDecodeOptions,
    Decoder as SigninumJpegDecoder, Downscale as SigninumDownscale,
    PixelFormat as SigninumPixelFormat,
};
use signinum_tilecodec::{
    DeflateCodec, DeflatePool, LzwCodec, LzwPool, TileDecompress, ZstdCodec, ZstdPool,
};

use crate::core::cache::CacheKey;
use crate::core::registry::{
    composite_region_from_source, crop_rgb_interleaved_u8_buffer, read_display_tile_from_source,
    SlideReader, DEFAULT_MAX_REGION_PIXELS,
};
use crate::core::types::*;
#[cfg(any(feature = "metal", feature = "cuda"))]
use crate::decode::jp2k::decode_batch_jp2k_pixels;
use crate::decode::jp2k::{decode_batch_jp2k, Jp2kColorSpace, Jp2kDecodeJob};
#[cfg(any(feature = "metal", feature = "cuda"))]
use crate::decode::jpeg::decode_batch_jpeg_pixels;
use crate::decode::jpeg::{decode_batch_jpeg, decode_jpeg_rgb_with_size_override, JpegDecodeJob};
use crate::error::WsiError;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::IfdId;
use crate::formats::tiff_family::layout::{
    DatasetLayout, StitchedLevelComponent, TileSource, TileSourceKey,
};

mod associated;
mod caches;
mod dct_reemit;
mod decode_batch;
mod dispatch;
mod image_ops;
mod jpeg_frame;
mod ndpi;
mod reader;
mod stitched;
mod synthetic;
mod tiled_ifd;

use caches::*;
use dct_reemit::encode_baseline_dct_image;
use decode_batch::*;
use image_ops::*;
use jpeg_frame::*;
pub(crate) use reader::TiffPixelReader;

#[cfg(feature = "metal")]
type MetalBackendSessionsRef<'a> = Option<&'a crate::output::metal::MetalBackendSessions>;
#[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "metal")))]
type MetalBackendSessionsRef<'a> = Option<&'a ()>;
#[cfg(feature = "cuda")]
type CudaBackendSessionsRef<'a> = Option<&'a crate::output::cuda::CudaBackendSessions>;
#[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "cuda")))]
type CudaBackendSessionsRef<'a> = Option<&'a ()>;

#[cfg(test)]
mod tests;
