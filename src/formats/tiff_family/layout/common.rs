//! Building blocks shared by the vendor layout interpreters: reading IFD
//! geometry, ordering and describing pyramid levels, and assembling the
//! single-scene datasets most vendors produce.

use std::collections::HashMap;

use crate::core::types::{
    AssociatedImage, AxesShape, Compression, Dataset, DatasetId, Level, SampleType, Scene, Series,
};
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::formats::tiff_family::icc::attach_source_icc_profile;
use crate::properties::Properties;

use super::{
    compute_tiff_dataset_id_and_record_quickhash, DatasetLayout, TileSource, TileSourceKey,
};

pub(super) fn compression_from_tag(val: u32) -> Compression {
    match val {
        1 => Compression::None,
        5 => Compression::Lzw,
        8 | 32946 => Compression::Deflate,
        6 | 7 => Compression::Jpeg,
        50000 => Compression::Zstd,
        22610 => Compression::JpegXr,
        33003 | 33005 => Compression::Jp2kYcbcr,
        33004 => Compression::Jp2kRgb,
        _ => Compression::Other(val as u16),
    }
}

/// Pixel area of an image for ordering levels by size. Widened to u128 because
/// IMAGE_WIDTH and IMAGE_LENGTH can be unbounded BigTIFF LONG8 values.
pub(super) fn level_area(width: u64, height: u64) -> u128 {
    u128::from(width) * u128::from(height)
}

/// Downsample of a `level`-sized pyramid level against a `base`-sized level 0:
/// the mean of its width and height ratios.
pub(super) fn averaged_downsample(base: (u64, u64), level: (u64, u64)) -> f64 {
    (base.0 as f64 / level.0 as f64 + base.1 as f64 / level.1 as f64) / 2.0
}

pub(super) fn single_scene_uint8_pyramid_dataset(
    dataset_id: DatasetId,
    axes: AxesShape,
    levels: Vec<Level>,
    associated_images: HashMap<String, AssociatedImage>,
    properties: Properties,
) -> Dataset {
    Dataset::new(
        dataset_id,
        vec![Scene::new(
            "s0",
            vec![Series::new(
                "ser0",
                axes,
                levels,
                SampleType::Uint8,
                Vec::new(),
            )],
        )],
    )
    .with_associated_images(associated_images)
    .with_properties(properties)
}

pub(super) fn regular_tiff_level(
    vendor: &str,
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
    downsample: f64,
) -> Result<Level, TiffParseError> {
    if tile_width == 0 || tile_height == 0 {
        return Err(TiffParseError::Structure(format!(
            "{vendor}: tile dimensions must be > 0 (got {tile_width}x{tile_height})"
        )));
    }
    let tiles_across = width.div_ceil(tile_width as u64);
    let tiles_down = height.div_ceil(tile_height as u64);

    Ok(Level {
        dimensions: (width, height),
        downsample,
        tile_layout: crate::core::types::TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        },
    })
}

#[derive(Clone)]
pub(super) struct TiledIfdDescriptor {
    pub(super) ifd_id: IfdId,
    pub(super) width: u64,
    pub(super) height: u64,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
    pub(super) compression: Compression,
    pub(super) jpeg_tables: Option<Vec<u8>>,
}

pub(super) fn tiled_ifd_descriptor(
    container: &TiffContainer,
    ifd_id: IfdId,
    compression: Compression,
) -> Result<TiledIfdDescriptor, TiffParseError> {
    Ok(TiledIfdDescriptor {
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
    })
}

/// Channel count of an associated image from its SamplesPerPixel tag, 3 when
/// the tag is absent. Values outside 1..=65535 clamp to that range rather
/// than wrapping into `u16`.
pub(super) fn associated_channels(container: &TiffContainer, ifd_id: IfdId) -> u16 {
    let samples = container
        .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
        .unwrap_or(3)
        .clamp(1, u32::from(u16::MAX));
    u16::try_from(samples).unwrap_or(u16::MAX)
}

pub(super) fn stripped_tile_source(
    container: &TiffContainer,
    ifd_id: IfdId,
    compression: Compression,
) -> Result<TileSource, TiffParseError> {
    Ok(TileSource::Stripped {
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
    })
}

pub(super) fn add_tiff_resolution_properties(
    container: &TiffContainer,
    ifd_id: IfdId,
    properties: &mut Properties,
) {
    let unit_microns = match container
        .get_u32(ifd_id, tags::RESOLUTION_UNIT)
        .unwrap_or(2)
    {
        3 => 10_000.0,
        _ => 25_400.0,
    };
    if let Ok(value) = container.get_f64(ifd_id, tags::X_RESOLUTION) {
        if value > 0.0 {
            properties.insert("openslide.mpp-x", (unit_microns / value).to_string());
        }
    }
    if let Ok(value) = container.get_f64(ifd_id, tags::Y_RESOLUTION) {
        if value > 0.0 {
            properties.insert("openslide.mpp-y", (unit_microns / value).to_string());
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finish_single_scene_uint8_tiff_layout<I>(
    container: &TiffContainer,
    lowest_resolution_ifd: IfdId,
    property_ifd: IfdId,
    axes: AxesShape,
    levels: Vec<Level>,
    associated_images: HashMap<String, AssociatedImage>,
    mut properties: Properties,
    tile_sources: HashMap<TileSourceKey, TileSource>,
    associated_sources: HashMap<String, TileSource>,
    source_icc_ifds: I,
) -> Result<DatasetLayout, TiffParseError>
where
    I: IntoIterator<Item = IfdId>,
{
    let dataset_id = compute_tiff_dataset_id_and_record_quickhash(
        container,
        lowest_resolution_ifd,
        property_ifd,
        &mut properties,
    )?;
    let mut dataset =
        single_scene_uint8_pyramid_dataset(dataset_id, axes, levels, associated_images, properties);
    attach_source_icc_profile(&mut dataset, container, source_icc_ifds, 0, 0)?;

    Ok(DatasetLayout {
        dataset,
        tile_sources,
        associated_sources,
    })
}
