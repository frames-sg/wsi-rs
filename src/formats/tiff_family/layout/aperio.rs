//! Aperio SVS layout interpreter.
//!
//! Classifies IFDs from an Aperio SVS TiffContainer into pyramid levels
//! (tiled IFDs) and associated images (stripped IFDs). Produces a
//! DatasetLayout with TileSource descriptors for each plane.

use std::collections::HashMap;

use crate::core::types::*;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;

use super::{
    compression_from_tag, finish_single_scene_uint8_tiff_layout, regular_tiff_level, DatasetLayout,
    TiffLayoutInterpreter, TileSource, TileSourceKey,
};

// ── AperioInterpreter ────────────────────────────────────────────

pub(crate) struct AperioInterpreter;

/// Intermediate: a tiled IFD classified as a pyramid level.
struct TiledIfd {
    ifd_id: IfdId,
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
    compression: Compression,
}

/// Intermediate: a stripped IFD classified as an associated image.
struct StrippedIfd {
    ifd_id: IfdId,
    ifd_index: usize,
    width: u32,
    height: u32,
    compression: Compression,
    strip_offsets: Vec<u64>,
    strip_byte_counts: Vec<u64>,
}

impl TiffLayoutInterpreter for AperioInterpreter {
    fn vendor_name(&self) -> &'static str {
        "aperio"
    }

    fn detect(&self, container: &TiffContainer) -> bool {
        let first_id = match container.top_ifds().first() {
            Some(&id) => id,
            None => return false,
        };

        // First top-level IFD must have TILE_WIDTH tag
        let ifd = match container.ifd_by_id(first_id) {
            Ok(ifd) => ifd,
            Err(_) => return false,
        };
        if !ifd.tags.contains_key(&tags::TILE_WIDTH) {
            return false;
        }

        // ImageDescription must start with "Aperio"
        match container.get_string(first_id, tags::IMAGE_DESCRIPTION) {
            Ok(desc) => desc.starts_with("Aperio"),
            Err(_) => false,
        }
    }

    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        let mut tiled_ifds: Vec<TiledIfd> = Vec::new();
        let mut stripped_ifds: Vec<StrippedIfd> = Vec::new();

        // Phase 1: Classify each top-level IFD as tiled (pyramid) or stripped (associated)
        for (idx, &ifd_id) in container.top_ifds().iter().enumerate() {
            let ifd = container.ifd_by_id(ifd_id)?;

            if ifd.tags.contains_key(&tags::TILE_WIDTH) {
                // Tiled IFD → pyramid level
                let width = container.get_u64(ifd_id, tags::IMAGE_WIDTH)?;
                let height = container.get_u64(ifd_id, tags::IMAGE_LENGTH)?;
                let tile_width = container.get_u32(ifd_id, tags::TILE_WIDTH)?;
                let tile_height = container.get_u32(ifd_id, tags::TILE_LENGTH)?;
                let comp_val = container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1);
                let compression = compression_from_tag(comp_val);

                tiled_ifds.push(TiledIfd {
                    ifd_id,
                    width,
                    height,
                    tile_width,
                    tile_height,
                    compression,
                });
            } else {
                // Stripped IFD → associated image
                let width =
                    u32::try_from(container.get_u64(ifd_id, tags::IMAGE_WIDTH).unwrap_or(0))
                        .unwrap_or(u32::MAX);
                let height =
                    u32::try_from(container.get_u64(ifd_id, tags::IMAGE_LENGTH).unwrap_or(0))
                        .unwrap_or(u32::MAX);
                let comp_val = container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1);
                let compression = compression_from_tag(comp_val);
                let strip_offsets = container
                    .get_u64_array(ifd_id, tags::STRIP_OFFSETS)
                    .map(|values| values.to_vec())
                    .unwrap_or_default();
                let strip_byte_counts = container
                    .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)
                    .map(|values| values.to_vec())
                    .unwrap_or_default();

                stripped_ifds.push(StrippedIfd {
                    ifd_id,
                    ifd_index: idx,
                    width,
                    height,
                    compression,
                    strip_offsets,
                    strip_byte_counts,
                });
            }
        }

        if tiled_ifds.is_empty() {
            return Err(TiffParseError::Structure(
                "No tiled pyramid levels found in Aperio SVS".into(),
            ));
        }

        // Phase 2: Sort tiled IFDs by area descending (largest = level 0)
        tiled_ifds.sort_by(|a, b| {
            let area_a = a.width * a.height;
            let area_b = b.width * b.height;
            area_b.cmp(&area_a)
        });

        // Some Aperio slides store different JPEG tables per pyramid level.
        let base_jpeg_tables = container
            .get_bytes(tiled_ifds[0].ifd_id, tags::JPEG_TABLES)
            .ok()
            .map(|b| b.to_vec());

        // Build levels and tile sources
        let base_w = tiled_ifds[0].width as f64;
        let base_h = tiled_ifds[0].height as f64;

        let mut levels = Vec::new();
        let mut tile_sources = HashMap::new();

        for (level_idx, tifd) in tiled_ifds.iter().enumerate() {
            let downsample = if level_idx == 0 {
                1.0
            } else {
                let dw = base_w / tifd.width as f64;
                let dh = base_h / tifd.height as f64;
                (dw + dh) / 2.0
            };

            levels.push(regular_tiff_level(
                "Aperio",
                tifd.width,
                tifd.height,
                tifd.tile_width,
                tifd.tile_height,
                downsample,
            )?);

            let key = TileSourceKey {
                scene: 0usize,
                series: 0usize,
                level: level_idx as u32,
                z: 0,
                c: 0,
                t: 0,
            };
            let jpeg_tables = if tifd.compression == Compression::Jpeg {
                container
                    .get_bytes(tifd.ifd_id, tags::JPEG_TABLES)
                    .ok()
                    .map(|bytes| bytes.to_vec())
                    .or_else(|| base_jpeg_tables.clone())
            } else {
                None
            };
            tile_sources.insert(
                key,
                TileSource::TiledIfd {
                    ifd_id: tifd.ifd_id,
                    jpeg_tables,
                    compression: tifd.compression,
                },
            );
        }

        // Phase 3: Classify stripped IFDs as associated images
        let mut associated_images: HashMap<String, AssociatedImage> = HashMap::new();
        let mut associated_sources: HashMap<String, TileSource> = HashMap::new();

        for sifd in &stripped_ifds {
            if sifd.width == 0 || sifd.height == 0 {
                continue;
            }

            let name = if sifd.ifd_index == 1 {
                "thumbnail".to_string()
            } else {
                // Check ImageDescription for "label" or "macro"
                container
                    .get_string(sifd.ifd_id, tags::IMAGE_DESCRIPTION)
                    .ok()
                    .and_then(|desc| {
                        let lower = desc.to_lowercase();
                        if lower.contains("label") {
                            Some("label".to_string())
                        } else if lower.contains("macro") {
                            Some("macro".to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| format!("image_{}", sifd.ifd_index))
            };

            let spp = container
                .get_u32(sifd.ifd_id, tags::SAMPLES_PER_PIXEL)
                .unwrap_or(3) as u16;

            associated_images.insert(
                name.clone(),
                AssociatedImage {
                    dimensions: (sifd.width, sifd.height),
                    sample_type: SampleType::Uint8,
                    channels: spp,
                },
            );
            associated_sources.insert(
                name,
                TileSource::Stripped {
                    ifd_id: sifd.ifd_id,
                    jpeg_tables: if sifd.compression == Compression::Jpeg {
                        container
                            .get_bytes(sifd.ifd_id, tags::JPEG_TABLES)
                            .ok()
                            .map(|bytes| bytes.to_vec())
                            .or_else(|| base_jpeg_tables.clone())
                    } else {
                        None
                    },
                    compression: sifd.compression,
                    strip_offsets: sifd.strip_offsets.clone(),
                    strip_byte_counts: sifd.strip_byte_counts.clone(),
                },
            );
        }

        // Phase 4: Parse properties from ImageDescription
        let properties = self.parse_properties(container)?;

        // Phase 5: Compute dataset ID
        let property_ifd = *container
            .top_ifds()
            .first()
            .ok_or_else(|| TiffParseError::Structure("No IFDs in Aperio container".into()))?;
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

impl AperioInterpreter {
    /// Parse Aperio's pipe-delimited ImageDescription into properties.
    ///
    /// Format: `"Aperio Image Library ...|Key1 = Value1|Key2 = Value2|..."`
    ///
    /// Segments after the first are split on `=` and stored as `aperio.{key}`.
    /// Standard compatibility properties are mapped from the Aperio-specific keys.
    fn parse_properties(&self, container: &TiffContainer) -> Result<Properties, TiffParseError> {
        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "aperio");

        let first_ifd = match container.top_ifds().first() {
            Some(&id) => id,
            None => return Ok(properties),
        };

        // Parse pipe-delimited ImageDescription
        if let Ok(desc) = container.get_string(first_ifd, tags::IMAGE_DESCRIPTION) {
            // Store raw description as openslide.comment
            properties.insert("openslide.comment", desc.to_string());

            // Split by '|', skip first segment (the "Aperio Image Library ..." prefix)
            let parts: Vec<&str> = desc.split('|').collect();
            for part in parts.iter().skip(1) {
                if let Some((key, value)) = part.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();
                    if !key.is_empty() && !value.is_empty() {
                        properties.insert(format!("aperio.{}", key), value.to_string());
                    }
                }
            }
        }

        // Map standard compatibility properties from Aperio keys.
        if let Some(mag) = properties.get("aperio.AppMag").map(|s| s.to_string()) {
            properties.insert("openslide.objective-power", mag);
        }
        if let Some(mpp) = properties.get("aperio.MPP").map(|s| s.to_string()) {
            properties.insert("openslide.mpp-x", mpp.clone());
            properties.insert("openslide.mpp-y", mpp);
        }

        Ok(properties)
    }
}

#[cfg(test)]
#[path = "aperio/tests/mod.rs"]
mod tests;
