//! Philips TIFF layout interpreter.
//!
//! Classifies IFDs from a Philips TiffContainer into pyramid levels
//! and associated images (label, macro). Produces a DatasetLayout
//! with TileSource descriptors for each plane.
//!
//! Philips TIFF files pad ImageWidth/ImageLength to tile boundaries.
//! The compatibility model exposes the public pyramid as an exact power-of-two downsample
//! chain from the base level, even when deeper TIFF IFD dimensions drift from
//! that logical pyramid. The XML metadata is still used for properties such as
//! MPP, but not for public level dimensions.

use std::collections::HashMap;

use crate::core::types::*;
use crate::decode::xml;
use crate::error::WsiError;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;
use base64::Engine;

use super::{
    compression_from_tag, finish_single_scene_uint8_tiff_layout, DatasetLayout,
    TiffLayoutInterpreter, TileSource, TileSourceKey,
};

// ── Constants ────────────────────────────────────────────────────────

/// TIFF tag 305 (Software) — not in the shared `tags::` constants.
const TAG_SOFTWARE: u16 = 305;

// ── PhilipsInterpreter ──────────────────────────────────────────────

pub(crate) struct PhilipsInterpreter;

impl TiffLayoutInterpreter for PhilipsInterpreter {
    fn vendor_name(&self) -> &'static str {
        "philips"
    }

    fn detect(&self, container: &TiffContainer) -> bool {
        let first_ifd = match container.top_ifds().first() {
            Some(&id) => id,
            None => return false,
        };

        // Check Software tag starts with "Philips"
        let software_ok = container
            .get_string(first_ifd, TAG_SOFTWARE)
            .map(|s| s.starts_with("Philips"))
            .unwrap_or(false);

        if !software_ok {
            return false;
        }

        // Check ImageDescription contains Philips XML markers
        let desc_ok = container
            .get_string(first_ifd, tags::IMAGE_DESCRIPTION)
            .map(|s| s.contains("<DataObject") && s.contains("DPUfsImport"))
            .unwrap_or(false);

        desc_ok
    }

    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        let mut tiled_ifds: Vec<TiledIfdInfo> = Vec::new();
        let mut associated_images: HashMap<String, AssociatedImage> = HashMap::new();
        let mut associated_sources: HashMap<String, TileSource> = HashMap::new();

        // Phase 1: Classify each top-level IFD as tiled (pyramid) or stripped (associated).
        for &ifd_id in container.top_ifds() {
            let ifd = container.ifd_by_id(ifd_id)?;

            let width = match container.get_u64(ifd_id, tags::IMAGE_WIDTH) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let height = match container.get_u64(ifd_id, tags::IMAGE_LENGTH) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if width == 0 || height == 0 {
                continue;
            }

            let is_tiled = ifd.tags.contains_key(&tags::TILE_WIDTH);

            if is_tiled {
                let tile_w = container.get_u32(ifd_id, tags::TILE_WIDTH).unwrap_or(256);
                let tile_h = container.get_u32(ifd_id, tags::TILE_LENGTH).unwrap_or(256);
                let comp_val = container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1);
                let compression = compression_from_tag(comp_val);

                let jpeg_tables = container
                    .get_bytes(ifd_id, tags::JPEG_TABLES)
                    .ok()
                    .map(|b| b.to_vec());

                tiled_ifds.push(TiledIfdInfo {
                    ifd_id,
                    width,
                    height,
                    tile_w,
                    tile_h,
                    compression,
                    jpeg_tables,
                });
            } else {
                // Stripped IFD — check ImageDescription for associated image type.
                let name = classify_associated(container, ifd_id);
                if let Some(name) = name {
                    let strip_offsets = container
                        .get_u64_array(ifd_id, tags::STRIP_OFFSETS)
                        .map(|values| values.to_vec())
                        .unwrap_or_default();
                    let strip_byte_counts = container
                        .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)
                        .map(|values| values.to_vec())
                        .unwrap_or_default();
                    let comp_val = container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1);
                    let compression = compression_from_tag(comp_val);
                    let jpeg_tables = if compression == Compression::Jpeg {
                        container
                            .get_bytes(ifd_id, tags::JPEG_TABLES)
                            .ok()
                            .map(<[u8]>::to_vec)
                    } else {
                        None
                    };

                    associated_images.insert(
                        name.clone(),
                        AssociatedImage {
                            dimensions: (
                                u32::try_from(width).unwrap_or(u32::MAX),
                                u32::try_from(height).unwrap_or(u32::MAX),
                            ),
                            sample_type: SampleType::Uint8,
                            channels: 3,
                            icc_profile: Vec::new(),
                        },
                    );
                    associated_sources.insert(
                        name,
                        TileSource::Stripped {
                            ifd_id,
                            jpeg_tables,
                            compression,
                            strip_offsets,
                            strip_byte_counts,
                        },
                    );
                }
            }
        }

        if tiled_ifds.is_empty() {
            return Err(TiffParseError::Structure(
                "No tiled pyramid levels found in Philips TIFF".into(),
            ));
        }

        // Sort tiled IFDs by area descending (largest = level 0).
        tiled_ifds.sort_by(|a, b| {
            let area_a = a.width * a.height;
            let area_b = b.width * b.height;
            area_b.cmp(&area_a)
        });

        // Phase 2: Extract DICOM_PIXEL_SPACING from XML for public properties.
        let spacings = extract_pixel_spacings(container, &tiled_ifds);
        let base_dims = (tiled_ifds[0].width, tiled_ifds[0].height);
        let base_spacing = spacings.as_ref().and_then(|s| s.first().copied());

        let mut levels = Vec::new();
        let mut tile_sources = HashMap::new();

        for (level_idx, info) in tiled_ifds.iter().enumerate() {
            let corrected_dims = philips_public_level_dimensions(base_dims, level_idx as u32);

            // Tiles across/down computed from the *padded* TIFF dimensions
            // (the real tile grid extent).
            if info.tile_w == 0 || info.tile_h == 0 {
                return Err(TiffParseError::Structure(format!(
                    "Philips: tile dimensions must be > 0 (got {}x{})",
                    info.tile_w, info.tile_h
                )));
            }
            let tiles_across = info.width.div_ceil(info.tile_w as u64);
            let tiles_down = info.height.div_ceil(info.tile_h as u64);

            let downsample = 2u64.pow(level_idx as u32) as f64;

            levels.push(Level {
                dimensions: corrected_dims,
                downsample,
                tile_layout: TileLayout::Regular {
                    tile_width: info.tile_w,
                    tile_height: info.tile_h,
                    tiles_across,
                    tiles_down,
                },
            });

            let key = TileSourceKey {
                scene: 0usize,
                series: 0usize,
                level: level_idx as u32,
                z: 0,
                c: 0,
                t: 0,
            };
            tile_sources.insert(
                key,
                TileSource::TiledIfd {
                    ifd_id: info.ifd_id,
                    jpeg_tables: info.jpeg_tables.clone(),
                    compression: info.compression,
                },
            );
        }

        // Phase 4: Parse properties from XML.
        let properties = parse_properties(container, base_spacing)?;

        // Phase 5: Compute dataset ID.
        let property_ifd = *container
            .top_ifds()
            .first()
            .ok_or_else(|| TiffParseError::Structure("No IFDs in Philips TIFF container".into()))?;
        finish_single_scene_uint8_tiff_layout(
            container,
            tiled_ifds.last().unwrap().ifd_id,
            property_ifd,
            AxesShape::default(),
            levels,
            associated_images,
            properties,
            tile_sources,
            associated_sources,
            tiled_ifds.iter().map(|ifd| ifd.ifd_id),
        )
    }
}

fn philips_public_level_dimensions(base_dims: (u64, u64), level_idx: u32) -> (u64, u64) {
    let factor = 1u64 << level_idx;
    ((base_dims.0 / factor).max(1), (base_dims.1 / factor).max(1))
}

// ── Internal types ──────────────────────────────────────────────────

/// Intermediate info for a tiled pyramid IFD.
struct TiledIfdInfo {
    ifd_id: IfdId,
    width: u64,
    height: u64,
    tile_w: u32,
    tile_h: u32,
    compression: Compression,
    jpeg_tables: Option<Vec<u8>>,
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Classify a stripped IFD as "label", "macro", or None.
/// Checks ImageDescription for case-insensitive match.
fn classify_associated(container: &TiffContainer, ifd_id: IfdId) -> Option<String> {
    let desc = container
        .get_string(ifd_id, tags::IMAGE_DESCRIPTION)
        .unwrap_or("");
    let lower = desc.to_ascii_lowercase();
    if lower.contains("label") {
        Some("label".to_string())
    } else if lower.contains("macro") {
        Some("macro".to_string())
    } else {
        None
    }
}

/// Extract per-level DICOM_PIXEL_SPACING values from the XML in ImageDescription.
///
/// Returns a Vec of spacing values (the first float in each pair) in the same
/// order as `tiled_ifds` (sorted by area descending). Returns None if XML
/// parsing fails or no spacings are found.
fn extract_pixel_spacings(
    container: &TiffContainer,
    tiled_ifds: &[TiledIfdInfo],
) -> Option<Vec<f64>> {
    let first_ifd = *container.top_ifds().first()?;
    let desc = container
        .get_string(first_ifd, tags::IMAGE_DESCRIPTION)
        .ok()?;

    let root = parse_philips_xml(desc).ok()?;

    let mut spacings_raw = extract_representation_spacings(&root).unwrap_or_default();
    if spacings_raw.is_empty() {
        collect_pixel_spacings(&root, &mut spacings_raw);

        if spacings_raw.is_empty() {
            return None;
        }

        // Sort raw spacings ascending (smallest spacing = highest resolution = level 0).
        spacings_raw.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    }

    // Match spacings to tiled IFDs. If counts differ, pad or truncate.
    let count = tiled_ifds.len();
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        if i < spacings_raw.len() {
            result.push(spacings_raw[i]);
        } else {
            // Fallback: extrapolate by doubling.
            let prev = result.last().copied().unwrap_or(1.0);
            result.push(prev * 2.0);
        }
    }

    Some(result)
}

fn extract_representation_spacings(node: &xml::XmlNode) -> Option<Vec<f64>> {
    let sequence = find_representation_sequence(node)?;
    let mut spacings = Vec::new();

    for representation in sequence.children.iter().filter(|child| {
        child.tag == "DataObject" && child.attr("ObjectType") == Some("PixelDataRepresentation")
    }) {
        for attribute in &representation.children {
            if attribute.tag == "Attribute" && attribute.attr("Name") == Some("DICOM_PIXEL_SPACING")
            {
                if let Some(text) = attribute.text.as_deref() {
                    if let Some(spacing) = parse_spacing(text) {
                        spacings.push(spacing);
                        break;
                    }
                }
            }
        }
    }

    (!spacings.is_empty()).then_some(spacings)
}

fn find_representation_sequence(node: &xml::XmlNode) -> Option<&xml::XmlNode> {
    if node.tag == "Attribute"
        && node.attr("Name") == Some("PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE")
    {
        return Some(node);
    }

    for child in &node.children {
        if let Some(sequence) = find_representation_sequence(child) {
            return Some(sequence);
        }
    }

    None
}

/// Recursively walk the XML tree collecting DICOM_PIXEL_SPACING values.
fn collect_pixel_spacings(node: &xml::XmlNode, out: &mut Vec<f64>) {
    if node.tag == "Attribute" {
        if let Some(name) = node.attr("Name") {
            if name == "DICOM_PIXEL_SPACING" {
                if let Some(text) = &node.text {
                    if let Some(spacing) = parse_spacing(text) {
                        out.push(spacing);
                    }
                }
            }
        }
    }
    for child in &node.children {
        collect_pixel_spacings(child, out);
    }
}

/// Parse a pixel spacing string and return the row/column spacing pair.
///
/// DICOM stores Pixel Spacing as row spacing first, then column spacing.
/// Compatibility metadata maps these to mpp-y and mpp-x respectively.
fn parse_spacing_pair(text: &str) -> Option<(f64, f64)> {
    let mut values = text
        .split_whitespace()
        .map(|value| value.trim_matches(|ch| matches!(ch, '"' | '\'' | ',')))
        .filter(|value| !value.is_empty())
        .filter_map(|value| value.parse::<f64>().ok())
        .filter(|value| *value > 0.0 && value.is_finite());

    let row_spacing = values.next()?;
    let column_spacing = values.next().unwrap_or(row_spacing);
    Some((row_spacing, column_spacing))
}

/// Parse a pixel spacing string like "0.000243 0.000243" and return the first value.
fn parse_spacing(text: &str) -> Option<f64> {
    parse_spacing_pair(text).map(|(row_spacing, _)| row_spacing)
}

fn resolve_mpp_pair(raw_spacing: Option<&str>, base_spacing: Option<f64>) -> Option<(f64, f64)> {
    if let Some(raw_spacing) = raw_spacing {
        if let Some((row_spacing, column_spacing)) = parse_spacing_pair(raw_spacing) {
            return Some((column_spacing * 1000.0, row_spacing * 1000.0));
        }
    }

    base_spacing.map(|spacing| {
        let mpp = spacing * 1000.0;
        (mpp, mpp)
    })
}

fn find_first_pixel_spacing(node: &xml::XmlNode) -> Option<&str> {
    if node.tag == "Attribute" && node.attr("Name") == Some("DICOM_PIXEL_SPACING") {
        if let Some(text) = node.text.as_deref() {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }

    for child in &node.children {
        if let Some(text) = find_first_pixel_spacing(child) {
            return Some(text);
        }
    }

    None
}

fn parse_philips_xml(description: &str) -> Result<xml::XmlNode, WsiError> {
    // Philips metadata may encode the quotes surrounding decimal strings as
    // predefined XML entities. The shared XML tree intentionally leaves text
    // references opaque, so normalize only this known vendor representation.
    if description.contains("&quot;") {
        xml::parse_xml(&description.replace("&quot;", "\""))
    } else {
        xml::parse_xml(description)
    }
}

/// Parse properties from the XML metadata.
fn parse_properties(
    container: &TiffContainer,
    base_spacing: Option<f64>,
) -> Result<Properties, TiffParseError> {
    let mut properties = Properties::new();
    properties.insert("openslide.vendor", "philips");

    let first_ifd = match container.top_ifds().first() {
        Some(&id) => id,
        None => return Ok(properties),
    };

    // ImageDescription -> openslide.comment
    if let Ok(desc) = container.get_string(first_ifd, tags::IMAGE_DESCRIPTION) {
        properties.insert("openslide.comment", desc.to_string());

        // Walk XML for Name/Value property pairs.
        if let Ok(root) = parse_philips_xml(desc) {
            let raw_mpp_spacing = find_first_pixel_spacing(&root);
            collect_xml_properties(&root, &mut properties);
            if let Some((mpp_x, mpp_y)) = resolve_mpp_pair(raw_mpp_spacing, base_spacing) {
                properties.insert("openslide.mpp-x", mpp_x.to_string());
                properties.insert("openslide.mpp-y", mpp_y.to_string());
            }
        }
    }

    // Software tag.
    if let Ok(sw) = container.get_string(first_ifd, TAG_SOFTWARE) {
        properties.insert("philips.Software", sw.to_string());
    }

    // MPP from DICOM_PIXEL_SPACING (multiply mm by 1000 -> micrometers).
    if properties.get("openslide.mpp-x").is_none() {
        if let Some((mpp_x, mpp_y)) = resolve_mpp_pair(None, base_spacing) {
            properties.insert("openslide.mpp-x", mpp_x.to_string());
            properties.insert("openslide.mpp-y", mpp_y.to_string());
        }
    }

    if let Some(encoded) = properties
        .get("philips.PIM_DP_UFS_BARCODE")
        .map(str::to_owned)
    {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) {
            if let Ok(value) = String::from_utf8(bytes) {
                let value = value.trim_end_matches('\0');
                if !value.is_empty() {
                    properties.insert("openslide.barcode", value);
                }
            }
        }
    }

    Ok(properties)
}

/// Walk DataObject tree extracting Attribute Name/text pairs as `philips.{Name}`.
fn collect_xml_properties(node: &xml::XmlNode, props: &mut Properties) {
    if node.tag == "Attribute" {
        if let Some(name) = node.attr("Name") {
            if let Some(text) = &node.text {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    props.insert(format!("philips.{}", name), trimmed.to_string());
                }
            }
        }
    }
    for child in &node.children {
        collect_xml_properties(child, props);
    }
}

#[cfg(test)]
#[path = "philips/tests.rs"]
mod tests;
