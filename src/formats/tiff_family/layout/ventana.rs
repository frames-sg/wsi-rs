//! Ventana BIF layout interpreter.
//!
//! Detects Ventana BIF files by checking for `<iScan` in the XMP tag (700)
//! of any top-level IFD. Builds an irregular tile grid from the embedded
//! XML tile layout metadata (`SlideStitchInfo` / `ImageInfo` / `TileJointInfo`).

use std::collections::HashMap;

use crate::core::types::*;
use crate::decode::xml;
use crate::formats::geometry::irregular_extra_tiles;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;

use super::{
    compression_from_tag, finish_single_scene_uint8_tiff_layout, DatasetLayout,
    TiffLayoutInterpreter, TileSource, TileSourceKey,
};

// ── VentanaInterpreter ──────────────────────────────────────────────

pub(crate) struct VentanaInterpreter;

mod geometry;
mod layout;
mod metadata;
mod stitching;

use geometry::{
    ventana_exact_tile_dimensions, ventana_level0_dimensions, ventana_public_level_dimensions,
};
#[cfg(test)]
use metadata::{extract_encode_info, extract_encode_info_bytes, extract_iscan_fragment_bytes};
use metadata::{find_encode_info_xml, find_xmp_string, has_iscan_xmp, parse_iscan_properties};
#[cfg(test)]
use stitching::{joint_delta, ventana_snake_coords, BifArea, BifTile};
use stitching::{parse_level0_xml, BifInfo};

#[cfg(test)]
#[path = "ventana/tests.rs"]
mod tests;
