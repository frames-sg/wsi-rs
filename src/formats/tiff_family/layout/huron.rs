//! Huron TIFF layout interpretation.

use std::collections::HashMap;

use crate::core::types::*;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;

use super::common::{
    add_tiff_resolution_properties, associated_channels, averaged_downsample, compression_from_tag,
    regular_tiff_level, stripped_tile_source, tiled_ifd_descriptor, ResolutionStyle,
    SingleSceneTiffLayout,
};
use super::{DatasetLayout, TiffLayoutInterpreter, TileSource, TileSourceKey};

pub(crate) struct HuronInterpreter;

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
                .get_string(first_ifd, tags::MAKE)
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
            if let Ok(depth) = container.get_u32(ifd_id, tags::IMAGE_DEPTH) {
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
                tiled.push(tiled_ifd_descriptor(container, ifd_id, compression)?);
                continue;
            }

            let name = if index == 1 {
                Some("thumbnail")
            } else {
                match container.get_u32(ifd_id, tags::SUBFILE_TYPE).ok() {
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

        let base_dims = (tiled[0].width, tiled[0].height);
        let mut levels = Vec::with_capacity(tiled.len());
        let mut tile_sources = HashMap::with_capacity(tiled.len());
        for (index, ifd) in tiled.iter().enumerate() {
            let downsample = if index == 0 {
                1.0
            } else {
                averaged_downsample(base_dims, (ifd.width, ifd.height))
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
                TileSourceKey::plane0(index as u32),
                TileSource::TiledIfd {
                    ifd_id: ifd.ifd_id,
                    jpeg_tables: ifd.jpeg_tables.clone(),
                    compression: ifd.compression,
                },
            );
        }

        let mut properties = parse_properties(description);
        add_tiff_resolution_properties(
            container,
            first_ifd,
            &mut properties,
            ResolutionStyle::INCH_FULL_PRECISION,
        );
        SingleSceneTiffLayout {
            lowest_resolution_ifd: tiled.last().unwrap().ifd_id,
            property_ifd: first_ifd,
            identity_extra_strings: &[],
            axes: AxesShape::default(),
            levels,
            associated_images,
            properties,
            tile_sources,
            associated_sources,
            source_icc_ifds: tiled.iter().map(|ifd| ifd.ifd_id),
        }
        .finish(container)
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
            associated_channels(container, ifd_id),
        ),
    );
    sources.insert(
        name.to_owned(),
        stripped_tile_source(container, ifd_id, compression)?,
    );
    Ok(())
}

#[cfg(test)]
#[path = "huron/tests.rs"]
mod tests;
