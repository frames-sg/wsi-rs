//! Huron TIFF layout interpretation.

use std::collections::HashMap;

use crate::core::types::*;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;

use super::{
    add_tiff_resolution_properties, compression_from_tag, finish_single_scene_uint8_tiff_layout,
    regular_tiff_level, DatasetLayout, TiffLayoutInterpreter, TileSource, TileSourceKey,
};

const TAG_SUBFILE_TYPE: u16 = 254;
const TAG_MAKE: u16 = 271;
const TAG_IMAGE_DEPTH: u16 = 32_997;

pub(crate) struct HuronInterpreter;

struct TiledIfd {
    ifd_id: IfdId,
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
    compression: Compression,
    jpeg_tables: Option<Vec<u8>>,
}

impl TiffLayoutInterpreter for HuronInterpreter {
    fn vendor_name(&self) -> &'static str {
        "huron"
    }

    fn detect(&self, container: &TiffContainer) -> bool {
        let Some(&first_ifd) = container.top_ifds().first() else {
            return false;
        };
        container
            .ifd_by_id(first_ifd)
            .is_ok_and(|ifd| ifd.tags.contains_key(&tags::TILE_WIDTH))
            && container
                .get_string(first_ifd, TAG_MAKE)
                .is_ok_and(|make| make.starts_with("Huron"))
    }

    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        let first_ifd = *container
            .top_ifds()
            .first()
            .ok_or_else(|| TiffParseError::Structure("Huron slide has no IFDs".into()))?;
        let description = container
            .get_string(first_ifd, tags::IMAGE_DESCRIPTION)
            .map_err(|_| TiffParseError::Structure("Huron ImageDescription is missing".into()))?;

        let mut tiled = Vec::new();
        let mut associated_images = HashMap::new();
        let mut associated_sources = HashMap::new();
        for (index, &ifd_id) in container.top_ifds().iter().enumerate() {
            if let Ok(depth) = container.get_u32(ifd_id, TAG_IMAGE_DEPTH) {
                if depth != 1 {
                    return Err(TiffParseError::Structure(format!(
                        "Huron IFD {ifd_id} has unsupported ImageDepth={depth}"
                    )));
                }
            }
            let ifd = container.ifd_by_id(ifd_id)?;
            let compression = compression_from_tag(container.get_u32(ifd_id, tags::COMPRESSION)?);
            super::argos::ensure_supported_compression("Huron", compression)?;
            if ifd.tags.contains_key(&tags::TILE_WIDTH) {
                tiled.push(TiledIfd {
                    ifd_id,
                    width: container.get_u64(ifd_id, tags::IMAGE_WIDTH)?,
                    height: container.get_u64(ifd_id, tags::IMAGE_LENGTH)?,
                    tile_width: container.get_u32(ifd_id, tags::TILE_WIDTH)?,
                    tile_height: container.get_u32(ifd_id, tags::TILE_LENGTH)?,
                    compression,
                    jpeg_tables: container
                        .get_bytes(ifd_id, tags::JPEG_TABLES)
                        .ok()
                        .map(<[u8]>::to_vec),
                });
                continue;
            }

            let name = if index == 1 {
                Some("thumbnail")
            } else {
                match container.get_u32(ifd_id, TAG_SUBFILE_TYPE).ok() {
                    Some(1) => Some("label"),
                    Some(9) => Some("macro"),
                    _ => None,
                }
            };
            if let Some(name) = name {
                insert_associated(
                    container,
                    ifd_id,
                    name,
                    compression,
                    &mut associated_images,
                    &mut associated_sources,
                )?;
            }
        }
        if tiled.is_empty() {
            return Err(TiffParseError::Structure(
                "Huron slide contains no tiled pyramid levels".into(),
            ));
        }

        let base_width = tiled[0].width as f64;
        let base_height = tiled[0].height as f64;
        let mut levels = Vec::with_capacity(tiled.len());
        let mut tile_sources = HashMap::with_capacity(tiled.len());
        for (index, ifd) in tiled.iter().enumerate() {
            let downsample = if index == 0 {
                1.0
            } else {
                ((base_width / ifd.width as f64) + (base_height / ifd.height as f64)) / 2.0
            };
            levels.push(regular_tiff_level(
                "Huron",
                ifd.width,
                ifd.height,
                ifd.tile_width,
                ifd.tile_height,
                downsample,
            )?);
            tile_sources.insert(
                TileSourceKey {
                    scene: 0,
                    series: 0,
                    level: index as u32,
                    z: 0,
                    c: 0,
                    t: 0,
                },
                TileSource::TiledIfd {
                    ifd_id: ifd.ifd_id,
                    jpeg_tables: ifd.jpeg_tables.clone(),
                    compression: ifd.compression,
                },
            );
        }

        let mut properties = parse_properties(description);
        add_tiff_resolution_properties(container, first_ifd, &mut properties);
        finish_single_scene_uint8_tiff_layout(
            container,
            tiled.last().unwrap().ifd_id,
            first_ifd,
            AxesShape::default(),
            levels,
            associated_images,
            properties,
            tile_sources,
            associated_sources,
            tiled.iter().map(|ifd| ifd.ifd_id),
        )
    }
}

fn parse_properties(description: &str) -> Properties {
    let mut properties = Properties::new();
    properties.insert("openslide.vendor", "huron");
    for line in description.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if !key.is_empty() {
            properties.insert(format!("huron.{key}"), value.to_owned());
        }
    }
    properties
}

fn insert_associated(
    container: &TiffContainer,
    ifd_id: IfdId,
    name: &str,
    compression: Compression,
    images: &mut HashMap<String, AssociatedImage>,
    sources: &mut HashMap<String, TileSource>,
) -> Result<(), TiffParseError> {
    let width = u32::try_from(container.get_u64(ifd_id, tags::IMAGE_WIDTH)?)
        .map_err(|_| TiffParseError::Structure(format!("Huron {name} width exceeds u32")))?;
    let height = u32::try_from(container.get_u64(ifd_id, tags::IMAGE_LENGTH)?)
        .map_err(|_| TiffParseError::Structure(format!("Huron {name} height exceeds u32")))?;
    if width == 0 || height == 0 {
        return Ok(());
    }
    images.insert(
        name.to_owned(),
        AssociatedImage::new(
            (width, height),
            SampleType::Uint8,
            container
                .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
                .unwrap_or(3) as u16,
        ),
    );
    sources.insert(
        name.to_owned(),
        TileSource::Stripped {
            ifd_id,
            jpeg_tables: container
                .get_bytes(ifd_id, tags::JPEG_TABLES)
                .ok()
                .map(<[u8]>::to_vec),
            compression,
            strip_offsets: container
                .get_u64_array(ifd_id, tags::STRIP_OFFSETS)?
                .to_vec(),
            strip_byte_counts: container
                .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)?
                .to_vec(),
        },
    );
    Ok(())
}

#[cfg(test)]
#[path = "huron/tests.rs"]
mod tests;
