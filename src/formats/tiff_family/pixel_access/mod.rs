//! Layer 3: Pixel access — TiffPixelReader and decode helpers.
//!
//! TiffPixelReader implements SlideReader by dispatching tile reads to focused
//! helper modules based on the `TileSource` variant.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use j2k_core::BackendRequest;
use j2k_jpeg::transcode::{
    extract_dct_blocks, DctExtractOptions, JpegDctCodingMode, JpegDctComponent, JpegDctImage,
};
use j2k_jpeg::{
    ColorTransform as J2kColorTransform, DecodeOptions as J2kDecodeOptions,
    DecodeRequest as J2kJpegDecodeRequest, Decoder as J2kJpegDecoder, Downscale as J2kDownscale,
    JpegView as J2kJpegView, PixelFormat as J2kPixelFormat,
};
use j2k_tilecodec::{
    DeflateCodec, DeflatePool, LzwCodec, LzwPool, TileDecompress, ZstdCodec, ZstdPool,
};
use lru::LruCache;
use rayon::prelude::*;

use crate::core::cache::CacheKey;
use crate::core::limits::{checked_product_to_usize, MAX_DECODED_IMAGE_BYTES};
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
use crate::formats::tiff_family::layout::{DatasetLayout, TileSource, TileSourceKey};

mod associated;
mod caches;
mod dct_reemit;
mod decode_batch;
mod dispatch;
mod image_ops;
mod jpeg_frame;
mod ndpi_batch;
mod ndpi_core;
mod ndpi_retile;
mod ndpi_tiles;
mod reader;
mod synthetic;
mod tiled_ifd;

use caches::*;
use dct_reemit::encode_baseline_dct_image;
use decode_batch::*;
use image_ops::*;
use jpeg_frame::*;
use ndpi_retile::*;
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
