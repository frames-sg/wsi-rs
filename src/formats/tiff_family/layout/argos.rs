//! ARGOS AVS layout interpretation.

use std::collections::HashMap;

use crate::core::types::*;
use crate::decode::xml::{self, XmlNode};
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::properties::Properties;

use super::common::{
    add_tiff_resolution_properties, associated_channels, averaged_downsample, checked_tile_grid,
    compression_from_tag, stripped_tile_source, tiled_ifd_descriptor, ResolutionStyle,
    SingleSceneTiffLayout, TiledIfdDescriptor as TiledIfd,
};
use super::{DatasetLayout, TiffLayoutInterpreter, TileSource, TileSourceKey};

const ARGOS_METADATA_TAG: u16 = 65_000;
const ARGOS_ROOT: &str = "Argos.Scan.Metadata";

pub(crate) struct ArgosInterpreter;

struct ParsedMetadata {
    xml: String,
    properties: Properties,
    z_planes: u32,
}

#[derive(Clone, Copy)]
struct OccupiedBounds {
    x: u64,
    y: u64,
    width: u64,
    height: u64,
}

struct SparseLevel {
    level: Level,
    occupied_bounds: Option<OccupiedBounds>,
}

type AssociatedImageSources = (
    HashMap<String, AssociatedImage>,
    HashMap<String, TileSource>,
);

impl TiffLayoutInterpreter for ArgosInterpreter {
    fn vendor_name(&self) -> &'static str {
        "argos"
    }

    fn detect(&self, container: &TiffContainer) -> bool {
        let Some(&first_ifd) = container.top_ifds().first() else {
            return false;
        };
        if !container
            .ifd_by_id(first_ifd)
            .is_ok_and(|ifd| ifd.tags.contains_key(&tags::TILE_WIDTH))
        {
            return false;
        }
        let Ok(raw) = container.get_string(first_ifd, ARGOS_METADATA_TAG) else {
            return false;
        };
        raw.contains(ARGOS_ROOT) && xml::parse_xml(raw).is_ok_and(|root| root.tag == ARGOS_ROOT)
    }

    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        let first_ifd = *container
            .top_ifds()
            .first()
            .ok_or_else(|| TiffParseError::Structure("ARGOS slide has no IFDs".into()))?;
        let metadata = parse_metadata(container, first_ifd)?;
        let tiled = collect_tiled_ifds(container)?;
        let stacks = group_z_stacks(&tiled);
        if stacks.len() != metadata.z_planes as usize {
            return Err(TiffParseError::Structure(format!(
                "ARGOS metadata declares {} Z planes but TIFF contains {} tiled pyramids",
                metadata.z_planes,
                stacks.len()
            )));
        }
        validate_stack_geometry(&stacks)?;

        // Public Z=0 and the OpenSlide ABI both select the middle physical
        // stack, so their shared level layout and occupied bounds must be
        // derived from that same stack.
        let middle_stack = (metadata.z_planes as usize - 1) / 2;
        let representative = &stacks[middle_stack];
        let base_dims = (representative[0].width, representative[0].height);
        let mut levels = Vec::with_capacity(representative.len());
        let mut level0_bounds = None;
        for (level_index, ifd) in representative.iter().enumerate() {
            let downsample = if level_index == 0 {
                1.0
            } else {
                averaged_downsample(base_dims, (ifd.width, ifd.height))
            };
            let sparse = sparse_level(container, ifd, downsample)?;
            if level_index == 0 {
                level0_bounds = sparse.occupied_bounds;
            }
            levels.push(sparse.level);
        }

        let normalized_stack_order = std::iter::once(middle_stack)
            .chain((0..stacks.len()).filter(|&index| index != middle_stack));
        let mut tile_sources = HashMap::with_capacity(tiled.len());
        for (z, physical_z) in normalized_stack_order.enumerate() {
            let stack = &stacks[physical_z];
            for (level, ifd) in stack.iter().enumerate() {
                tile_sources.insert(
                    TileSourceKey {
                        scene: 0,
                        series: 0,
                        level: level as u32,
                        z: z as u32,
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
        }

        let (associated_images, associated_sources) = associated_images(container)?;
        let lowest_ifd = stacks[middle_stack]
            .last()
            .ok_or_else(|| TiffParseError::Structure("ARGOS pyramid has no levels".into()))?
            .ifd_id;
        let mut properties = metadata.properties;
        add_tiff_resolution_properties(
            container,
            representative[0].ifd_id,
            &mut properties,
            ResolutionStyle::INCH_FULL_PRECISION,
        );
        add_sparse_bounds_properties(
            &mut properties,
            level0_bounds,
            representative[0].width,
            representative[0].height,
        );

        SingleSceneTiffLayout {
            lowest_resolution_ifd: lowest_ifd,
            property_ifd: first_ifd,
            identity_extra_strings: &[metadata.xml.as_str()],
            axes: AxesShape::new(metadata.z_planes, 1, 1),
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

fn parse_metadata(
    container: &TiffContainer,
    first_ifd: IfdId,
) -> Result<ParsedMetadata, TiffParseError> {
    let raw = container
        .get_string(first_ifd, ARGOS_METADATA_TAG)
        .map_err(|_| TiffParseError::Structure("ARGOS metadata tag 65000 is missing".into()))?;
    let root = xml::parse_xml(raw).map_err(|error| {
        TiffParseError::Structure(format!("failed to parse ARGOS metadata XML: {error}"))
    })?;
    if root.tag != ARGOS_ROOT {
        return Err(TiffParseError::Structure(format!(
            "ARGOS metadata root must be {ARGOS_ROOT}, got {}",
            root.tag
        )));
    }

    let mut properties = Properties::new();
    properties.insert("openslide.vendor", "argos");
    collect_leaf_properties(&root, "argos", &mut properties);

    let min_z = parse_z_property(&properties, "argos.MinZ")?;
    let max_z = parse_z_property(&properties, "argos.MaxZ")?;
    let plane_count = max_z
        .checked_sub(min_z)
        .and_then(|span| span.checked_add(1))
        .ok_or_else(|| TiffParseError::Structure("ARGOS focal-plane range is invalid".into()))?;
    let z_planes = u32::try_from(plane_count)
        .map_err(|_| TiffParseError::Structure("ARGOS focal-plane count exceeds u32".into()))?;
    if z_planes == 0 {
        return Err(TiffParseError::Structure(
            "ARGOS focal-plane count must be positive".into(),
        ));
    }

    if let Some(objective) = properties
        .get("argos.ObjectiveMagnification")
        .and_then(|value| value.trim().parse::<i64>().ok())
    {
        properties.insert("openslide.objective-power", objective.to_string());
    }
    if let Some(barcode) = properties.get("argos.Barcode").map(str::to_owned) {
        if !barcode.is_empty() {
            properties.insert("openslide.barcode", barcode);
        }
    }

    Ok(ParsedMetadata {
        xml: raw.to_owned(),
        properties,
        z_planes,
    })
}

fn parse_z_property(properties: &Properties, key: &str) -> Result<i64, TiffParseError> {
    properties
        .get(key)
        .ok_or_else(|| TiffParseError::Structure(format!("ARGOS metadata is missing {key}")))?
        .trim()
        .parse::<i64>()
        .map_err(|_| TiffParseError::Structure(format!("ARGOS metadata has invalid {key}")))
}

fn collect_leaf_properties(node: &XmlNode, prefix: &str, properties: &mut Properties) {
    for child in &node.children {
        let key = format!("{prefix}.{}", child.tag);
        collect_leaf_properties(child, &key, properties);
        if child.attributes.is_empty() && child.children.is_empty() {
            if let Some(value) = child.text.as_deref() {
                properties.insert(key, value.to_owned());
            }
        }
    }
}

fn collect_tiled_ifds(container: &TiffContainer) -> Result<Vec<TiledIfd>, TiffParseError> {
    let mut tiled = Vec::new();
    for &ifd_id in container.top_ifds() {
        let ifd = container.ifd_by_id(ifd_id)?;
        if !ifd.tags.contains_key(&tags::TILE_WIDTH) {
            continue;
        }
        let compression = compression_from_tag(container.get_u32(ifd_id, tags::COMPRESSION)?);
        ensure_supported_compression("ARGOS", compression)?;
        tiled.push(tiled_ifd_descriptor(container, ifd_id, compression)?);
    }
    if tiled.is_empty() {
        return Err(TiffParseError::Structure(
            "ARGOS slide contains no tiled pyramid levels".into(),
        ));
    }
    Ok(tiled)
}

fn group_z_stacks(tiled: &[TiledIfd]) -> Vec<Vec<TiledIfd>> {
    let mut stacks: Vec<Vec<TiledIfd>> = Vec::new();
    let mut previous_width = None;
    for ifd in tiled {
        if previous_width.is_some_and(|width| ifd.width >= width) {
            stacks.push(Vec::new());
        }
        if stacks.is_empty() {
            stacks.push(Vec::new());
        }
        stacks.last_mut().unwrap().push(ifd.clone());
        previous_width = Some(ifd.width);
    }
    stacks
}

fn validate_stack_geometry(stacks: &[Vec<TiledIfd>]) -> Result<(), TiffParseError> {
    let Some(reference) = stacks.first() else {
        return Err(TiffParseError::Structure("ARGOS has no Z stacks".into()));
    };
    if reference.is_empty() {
        return Err(TiffParseError::Structure(
            "ARGOS Z stack has no levels".into(),
        ));
    }
    for (z, stack) in stacks.iter().enumerate() {
        if stack.len() != reference.len() {
            return Err(TiffParseError::Structure(format!(
                "ARGOS Z plane {z} has {} levels; expected {}",
                stack.len(),
                reference.len()
            )));
        }
        for (level, (actual, expected)) in stack.iter().zip(reference).enumerate() {
            if actual.width != expected.width
                || actual.height != expected.height
                || actual.tile_width != expected.tile_width
                || actual.tile_height != expected.tile_height
            {
                return Err(TiffParseError::Structure(format!(
                    "ARGOS Z plane {z} level {level} geometry differs from the first plane"
                )));
            }
        }
    }
    Ok(())
}

fn sparse_level(
    container: &TiffContainer,
    ifd: &TiledIfd,
    downsample: f64,
) -> Result<SparseLevel, TiffParseError> {
    let (tiles_across, _, expected) = checked_tile_grid(
        "ARGOS",
        ifd.width,
        ifd.height,
        ifd.tile_width,
        ifd.tile_height,
    )?;
    let offsets = container.get_u64_array(ifd.ifd_id, tags::TILE_OFFSETS)?;
    let byte_counts = container.get_u64_array(ifd.ifd_id, tags::TILE_BYTE_COUNTS)?;
    if offsets.len() != expected || byte_counts.len() != expected {
        return Err(TiffParseError::Structure(format!(
            "ARGOS IFD {} tile arrays have lengths offsets={} byte_counts={}, expected {expected}",
            ifd.ifd_id,
            offsets.len(),
            byte_counts.len()
        )));
    }

    let mut tiles = HashMap::new();
    let mut occupied_tile_bounds: Option<(u64, u64, u64, u64)> = None;
    for (index, (&offset, &byte_count)) in offsets.iter().zip(byte_counts).enumerate() {
        if offset == 0 || byte_count == 0 {
            continue;
        }
        let row = index as u64 / tiles_across;
        let col = index as u64 % tiles_across;
        let right = col
            .checked_add(1)
            .ok_or_else(|| TiffParseError::Structure("ARGOS occupied tile X overflow".into()))?;
        let bottom = row
            .checked_add(1)
            .ok_or_else(|| TiffParseError::Structure("ARGOS occupied tile Y overflow".into()))?;
        occupied_tile_bounds = Some(match occupied_tile_bounds {
            Some((left, top, previous_right, previous_bottom)) => (
                left.min(col),
                top.min(row),
                previous_right.max(right),
                previous_bottom.max(bottom),
            ),
            None => (col, row, right, bottom),
        });
        tiles.insert(
            (col as i64, row as i64),
            TileEntry {
                offset: (0.0, 0.0),
                dimensions: (ifd.tile_width, ifd.tile_height),
                tiff_tile_index: Some(index),
                extent: Default::default(),
            },
        );
    }

    let occupied_bounds = occupied_tile_bounds
        .map(
            |(left, top, right, bottom)| -> Result<OccupiedBounds, TiffParseError> {
                let x = left
                    .checked_mul(u64::from(ifd.tile_width))
                    .ok_or_else(|| TiffParseError::Structure("ARGOS occupied X overflow".into()))?;
                let y = top
                    .checked_mul(u64::from(ifd.tile_height))
                    .ok_or_else(|| TiffParseError::Structure("ARGOS occupied Y overflow".into()))?;
                let right = right
                    .checked_mul(u64::from(ifd.tile_width))
                    .ok_or_else(|| {
                        TiffParseError::Structure("ARGOS occupied width overflow".into())
                    })?
                    .min(ifd.width);
                let bottom = bottom
                    .checked_mul(u64::from(ifd.tile_height))
                    .ok_or_else(|| {
                        TiffParseError::Structure("ARGOS occupied height overflow".into())
                    })?
                    .min(ifd.height);
                Ok(OccupiedBounds {
                    x,
                    y,
                    width: right.checked_sub(x).ok_or_else(|| {
                        TiffParseError::Structure("ARGOS occupied width underflow".into())
                    })?,
                    height: bottom.checked_sub(y).ok_or_else(|| {
                        TiffParseError::Structure("ARGOS occupied height underflow".into())
                    })?,
                })
            },
        )
        .transpose()?;

    Ok(SparseLevel {
        level: Level {
            dimensions: (ifd.width, ifd.height),
            downsample,
            tile_layout: TileLayout::Irregular {
                tile_advance: (f64::from(ifd.tile_width), f64::from(ifd.tile_height)),
                extra_tiles: (0, 0, 0, 0),
                tiles,
            },
        },
        occupied_bounds,
    })
}

fn add_sparse_bounds_properties(
    properties: &mut Properties,
    bounds: Option<OccupiedBounds>,
    level_width: u64,
    level_height: u64,
) {
    let Some(bounds) = bounds else {
        return;
    };
    if bounds.x == 0
        && bounds.y == 0
        && bounds.width == level_width
        && bounds.height == level_height
    {
        return;
    }
    properties.insert("openslide.bounds-x", bounds.x.to_string());
    properties.insert("openslide.bounds-y", bounds.y.to_string());
    properties.insert("openslide.bounds-width", bounds.width.to_string());
    properties.insert("openslide.bounds-height", bounds.height.to_string());
}

fn associated_images(container: &TiffContainer) -> Result<AssociatedImageSources, TiffParseError> {
    let mut images = HashMap::new();
    let mut sources = HashMap::new();
    let count = container.top_ifds().len();
    for (index, &ifd_id) in container.top_ifds().iter().enumerate() {
        if container
            .ifd_by_id(ifd_id)?
            .tags
            .contains_key(&tags::TILE_WIDTH)
        {
            continue;
        }
        let name = match count.checked_sub(index) {
            Some(2) => "thumbnail",
            Some(1) => "macro",
            _ => continue,
        };
        insert_stripped_associated(container, ifd_id, name, &mut images, &mut sources)?;
    }
    Ok((images, sources))
}

fn insert_stripped_associated(
    container: &TiffContainer,
    ifd_id: IfdId,
    name: &str,
    images: &mut HashMap<String, AssociatedImage>,
    sources: &mut HashMap<String, TileSource>,
) -> Result<(), TiffParseError> {
    let width = u32::try_from(container.get_u64(ifd_id, tags::IMAGE_WIDTH)?)
        .map_err(|_| TiffParseError::Structure(format!("ARGOS {name} width exceeds u32")))?;
    let height = u32::try_from(container.get_u64(ifd_id, tags::IMAGE_LENGTH)?)
        .map_err(|_| TiffParseError::Structure(format!("ARGOS {name} height exceeds u32")))?;
    if width == 0 || height == 0 {
        return Ok(());
    }
    let compression = compression_from_tag(container.get_u32(ifd_id, tags::COMPRESSION)?);
    ensure_supported_compression("ARGOS", compression)?;
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

pub(super) fn ensure_supported_compression(
    vendor: &str,
    compression: Compression,
) -> Result<(), TiffParseError> {
    match compression {
        Compression::None
        | Compression::Lzw
        | Compression::Deflate
        | Compression::Jpeg
        | Compression::Jp2kRgb
        | Compression::Jp2kYcbcr
        | Compression::Zstd => Ok(()),
        other => Err(TiffParseError::Structure(format!(
            "{vendor} uses unsupported TIFF compression {other:?}"
        ))),
    }
}

#[cfg(test)]
#[path = "argos/tests.rs"]
mod tests;
