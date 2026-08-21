//! Generic TIFF layout interpreter.
//!
//! Fallback interpreter for tiled TIFFs and a narrow set of strip-based RGB
//! TIFFs that are not claimed by a vendor-specific interpreter. Registered
//! last in the interpreter chain so it only fires when all specific vendors
//! decline.

use std::collections::HashMap;

use crate::core::limits::MAX_DECODED_IMAGE_BYTES;
use crate::core::types::*;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;

use super::{
    compression_from_tag, finish_single_scene_uint8_tiff_layout, regular_tiff_level, DatasetLayout,
    TiffLayoutInterpreter, TileSource, TileSourceKey,
};

// ── Interpreter ──────────────────────────────────────────────────────

pub(crate) struct GenericTiffInterpreter;

mod layout;
mod support;

use support::is_supported_stripped_rgb_ifd;

#[cfg(test)]
#[path = "generic/tests.rs"]
mod tests;
