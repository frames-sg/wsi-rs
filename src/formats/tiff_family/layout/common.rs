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
    compute_tiff_dataset_identity_with_extra_strings, DatasetLayout, TileSource, TileSourceKey,
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

fn single_scene_uint8_pyramid_dataset(
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

/// Most tiles one irregular-layout level may declare; each becomes a
/// `TileEntry` in the level's tile map.
pub(super) const MAX_IRREGULAR_TILES_PER_LEVEL: u64 = 1_000_000;

/// `(tiles_across, tiles_down, tile_count)` of a `width`x`height` image cut
/// into `tile_width`x`tile_height` tiles. Zero sizes, an overflowing count
/// and grids above [`MAX_IRREGULAR_TILES_PER_LEVEL`] are errors.
pub(super) fn checked_tile_grid(
    vendor: &str,
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
) -> Result<(u64, u64, usize), TiffParseError> {
    if width == 0 || height == 0 || tile_width == 0 || tile_height == 0 {
        return Err(TiffParseError::Structure(format!(
            "{vendor} image and tile dimensions must be non-zero (image={width}x{height}, tile={tile_width}x{tile_height})"
        )));
    }
    let tiles_across = width.div_ceil(u64::from(tile_width));
    let tiles_down = height.div_ceil(u64::from(tile_height));
    let tile_count = tiles_across
        .checked_mul(tiles_down)
        .ok_or_else(|| TiffParseError::Structure(format!("{vendor} tile count overflow")))?;
    if tile_count > MAX_IRREGULAR_TILES_PER_LEVEL {
        return Err(TiffParseError::Structure(format!(
            "{vendor} level declares {tile_count} tiles, exceeding the {MAX_IRREGULAR_TILES_PER_LEVEL}-tile limit"
        )));
    }
    let tile_count = usize::try_from(tile_count).map_err(|_| {
        TiffParseError::Structure(format!("{vendor} tile count exceeds address space"))
    })?;
    Ok((tiles_across, tiles_down, tile_count))
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

/// Rejects compressions the TIFF pixel reader cannot decode, for vendors
/// that validate them up front.
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

/// `base`, or the first of `base-1`, `base-2`, ... not yet in `existing`.
pub(super) fn unique_associated_name(
    existing: &HashMap<String, AssociatedImage>,
    base: &str,
) -> String {
    if !existing.contains_key(base) {
        return base.into();
    }
    (1..)
        .map(|index| format!("{base}-{index}"))
        .find(|candidate| !existing.contains_key(candidate))
        .unwrap_or_else(|| unreachable!("unbounded suffix search must eventually return"))
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

/// How a vendor turns XResolution/YResolution into `openslide.mpp-*`.
#[derive(Clone, Copy)]
pub(super) struct ResolutionStyle {
    /// ResolutionUnit assumed when the tag is absent: 3 is centimeters, any
    /// other value inches.
    pub(super) default_unit: u32,
    /// Fixed decimal places, or `None` for the shortest exact form.
    pub(super) decimals: Option<usize>,
}

impl ResolutionStyle {
    /// Inch default and full precision.
    pub(super) const INCH_FULL_PRECISION: Self = Self {
        default_unit: 2,
        decimals: None,
    };
}

pub(super) fn add_tiff_resolution_properties(
    container: &TiffContainer,
    ifd_id: IfdId,
    properties: &mut Properties,
    style: ResolutionStyle,
) {
    let unit_microns = match container
        .get_u32(ifd_id, tags::RESOLUTION_UNIT)
        .unwrap_or(style.default_unit)
    {
        3 => 10_000.0,
        _ => 25_400.0,
    };
    let format = |mpp: f64| match style.decimals {
        Some(decimals) => format!("{mpp:.decimals$}"),
        None => mpp.to_string(),
    };
    if let Ok(value) = container.get_f64(ifd_id, tags::X_RESOLUTION) {
        if value > 0.0 {
            properties.insert("openslide.mpp-x", format(unit_microns / value));
        }
    }
    if let Ok(value) = container.get_f64(ifd_id, tags::Y_RESOLUTION) {
        if value > 0.0 {
            properties.insert("openslide.mpp-y", format(unit_microns / value));
        }
    }
}

/// A single-scene, single-series 8-bit pyramid, ready to become a
/// `DatasetLayout` once its identity is hashed and its ICC profile attached.
pub(super) struct SingleSceneTiffLayout<'a, I> {
    pub(super) lowest_resolution_ifd: IfdId,
    pub(super) property_ifd: IfdId,
    /// Vendor strings hashed into the dataset identity after the TIFF
    /// string properties.
    pub(super) identity_extra_strings: &'a [&'a str],
    pub(super) axes: AxesShape,
    pub(super) levels: Vec<Level>,
    pub(super) associated_images: HashMap<String, AssociatedImage>,
    pub(super) properties: Properties,
    pub(super) tile_sources: HashMap<TileSourceKey, TileSource>,
    pub(super) associated_sources: HashMap<String, TileSource>,
    /// IFDs searched, in order, for the series' source ICC profile.
    pub(super) source_icc_ifds: I,
}

impl<I> SingleSceneTiffLayout<'_, I>
where
    I: IntoIterator<Item = IfdId>,
{
    /// Hashes the dataset identity, records `openslide.quickhash-1` when the
    /// lowest level is small enough to hash, and attaches the ICC profile.
    pub(super) fn finish(self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
        let identity = compute_tiff_dataset_identity_with_extra_strings(
            container,
            self.lowest_resolution_ifd,
            self.property_ifd,
            self.identity_extra_strings,
        )?;
        let mut properties = self.properties;
        if let Some(quickhash1) = identity.quickhash1.as_deref() {
            properties.insert("openslide.quickhash-1", quickhash1);
        }
        let mut dataset = single_scene_uint8_pyramid_dataset(
            identity.dataset_id,
            self.axes,
            self.levels,
            self.associated_images,
            properties,
        );
        attach_source_icc_profile(&mut dataset, container, self.source_icc_ifds, 0, 0)?;

        Ok(DatasetLayout {
            dataset,
            tile_sources: self.tile_sources,
            associated_sources: self.associated_sources,
        })
    }
}
