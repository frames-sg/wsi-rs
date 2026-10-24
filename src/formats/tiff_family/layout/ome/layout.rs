//! Maps a parsed OME document onto the container's IFDs.

use std::collections::HashMap;

use tracing::warn;

use crate::core::types::*;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::formats::tiff_family::icc::attach_source_icc_profile;
use crate::properties::Properties;

use super::super::common::{compression_from_tag, regular_tiff_level};
use super::super::{compute_tiff_dataset_identity, DatasetLayout, TileSource, TileSourceKey};
use super::metadata::{parse_ome_xml, OmeDocument, OmeImage, OmePixels, PhysicalSize, PlaneShape};
use super::VENDOR;

/// An OME image whose planes are resolved to top-level IFDs.
struct MappedImage<'a> {
    index: usize,
    image: &'a OmeImage,
    shape: PlaneShape,
    sample_type: SampleType,
    samples_per_pixel: u32,
    /// Top-level IFD of each plane, by linear plane index.
    planes: Vec<IfdId>,
}

/// Top-level IFD bookkeeping shared by every image in the document.
struct IfdUsage {
    /// First IFD after the previous image's planes, for images without TiffData.
    next: usize,
    /// Whether some plane already uses each top-level IFD.
    used: Vec<bool>,
}

struct BuiltSeries {
    series: Series,
    /// Level IFDs of plane 0, full resolution first.
    plane0_levels: Vec<IfdId>,
    /// Every IFD referenced by the series' tile sources.
    ifds: Vec<IfdId>,
}

pub(super) fn interpret(container: &TiffContainer) -> Result<DatasetLayout, TiffParseError> {
    let property_ifd = *container
        .top_ifds()
        .first()
        .ok_or_else(|| TiffParseError::Structure("OME-TIFF has no IFDs".into()))?;
    let description = container.get_string(property_ifd, tags::IMAGE_DESCRIPTION)?;
    let document = parse_ome_xml(description)?;
    if document.binary_only {
        return Err(TiffParseError::Structure(
            "OME-TIFF metadata lives in an external companion file (BinaryOnly), which is not supported"
                .into(),
        ));
    }

    let own_file_name = container.path().file_name().and_then(|name| name.to_str());
    let mut ifd_usage = IfdUsage {
        next: 0,
        used: vec![false; container.top_ifds().len()],
    };
    let mut main = Vec::new();
    let mut associated = Vec::new();
    for (index, image) in document.images.iter().enumerate() {
        let mapped = map_image(
            container,
            &document,
            index,
            image,
            own_file_name,
            &mut ifd_usage,
        )?;
        match associated_name(&mapped, document.images.len()) {
            Some(name) => associated.push((name, mapped)),
            None => main.push(mapped),
        }
    }

    let mut tile_sources = HashMap::new();
    let mut scenes = Vec::with_capacity(main.len());
    let mut scene_ifds = Vec::with_capacity(main.len());
    let mut lowest_resolution_ifd = None;
    for (scene_index, image) in main.iter().enumerate() {
        let built = build_series(container, scene_index, image, &mut tile_sources)?;
        lowest_resolution_ifd
            .get_or_insert(*built.plane0_levels.last().unwrap_or(&image.planes[0]));
        let scene = Scene::new(format!("s{scene_index}"), vec![built.series]);
        scenes.push(match image.image.name.as_deref() {
            Some(name) => scene.with_name(name),
            None => scene,
        });
        scene_ifds.push(built.ifds);
    }
    let lowest_resolution_ifd = lowest_resolution_ifd
        .ok_or_else(|| TiffParseError::Structure("OME-TIFF has no main image".into()))?;

    let mut associated_images = HashMap::new();
    let mut associated_sources = HashMap::new();
    for (base_name, image) in &associated {
        let ifd_id = image.planes[0];
        let (width, height) = ifd_dimensions(container, ifd_id)?;
        let name = unique_name(&associated_images, base_name);
        associated_images.insert(
            name.clone(),
            AssociatedImage::new(
                (
                    u32::try_from(width).unwrap_or(u32::MAX),
                    u32::try_from(height).unwrap_or(u32::MAX),
                ),
                image.sample_type,
                u16::try_from(image.samples_per_pixel).unwrap_or(u16::MAX),
            ),
        );
        associated_sources.insert(name, associated_source(container, ifd_id));
    }

    let mut properties = ome_properties(&document, description, &main);
    let identity = compute_tiff_dataset_identity(container, lowest_resolution_ifd, property_ifd)?;
    if let Some(quickhash1) = identity.quickhash1.as_deref() {
        properties.insert("openslide.quickhash-1", quickhash1);
    }
    let mut dataset = Dataset::new(identity.dataset_id, scenes)
        .with_associated_images(associated_images)
        .with_properties(properties);
    for (scene_index, ifds) in scene_ifds.iter().enumerate() {
        attach_source_icc_profile(&mut dataset, container, ifds, scene_index, 0)?;
    }

    Ok(DatasetLayout {
        dataset,
        tile_sources,
        associated_sources,
    })
}

fn map_image<'a>(
    container: &TiffContainer,
    document: &OmeDocument,
    index: usize,
    image: &'a OmeImage,
    own_file_name: Option<&str>,
    ifd_usage: &mut IfdUsage,
) -> Result<MappedImage<'a>, TiffParseError> {
    let context = format!("OME-TIFF Image {index}");
    let pixels = &image.pixels;
    let top_ifds = container.top_ifds();

    if let Some(external) = pixels
        .tiff_data
        .iter()
        .find(|tiff_data| tiff_data.is_external(document.uuid.as_deref(), own_file_name))
    {
        return Err(TiffParseError::Structure(format!(
            "{context} stores planes in {}; multi-file OME-TIFF datasets are not supported",
            external.file_name.as_deref().unwrap_or("another file")
        )));
    }

    let sample_type = match pixels.pixel_type.as_str() {
        "uint8" => SampleType::Uint8,
        "uint16" => SampleType::Uint16,
        "float" => SampleType::Float32,
        other => {
            return Err(TiffParseError::Structure(format!(
                "{context} has pixel type '{other}'; only uint8, uint16 and float are supported"
            )));
        }
    };

    // Channel elements usually declare SamplesPerPixel; otherwise trust the
    // first plane's IFD.
    let samples_per_pixel = match pixels.channels.first().and_then(|ch| ch.samples_per_pixel) {
        Some(samples) => samples,
        None => {
            let first_ifd = pixels
                .tiff_data
                .first()
                .map_or(Some(ifd_usage.next), |tiff_data| {
                    usize::try_from(tiff_data.ifd.unwrap_or(0)).ok()
                });
            first_ifd
                .and_then(|ifd_index| top_ifds.get(ifd_index))
                .and_then(|&ifd_id| container.get_u32(ifd_id, tags::SAMPLES_PER_PIXEL).ok())
                .unwrap_or(1)
        }
    };
    if !matches!(samples_per_pixel, 1 | 3) || !pixels.size_c.is_multiple_of(samples_per_pixel) {
        return Err(TiffParseError::Structure(format!(
            "{context} has {samples_per_pixel} samples per pixel with SizeC {}; \
             only 1 or 3 samples that divide SizeC are supported",
            pixels.size_c
        )));
    }

    let shape = PlaneShape {
        z: pixels.size_z,
        c: pixels.size_c / samples_per_pixel,
        t: pixels.size_t,
        order: pixels.dimension_order,
    };
    let plane_indices = map_planes(&context, pixels, shape, top_ifds.len(), ifd_usage.next)?;
    for &ifd_index in &plane_indices {
        // Planes never share an IFD; refusing reuse also bounds the image count.
        if std::mem::replace(&mut ifd_usage.used[ifd_index], true) {
            return Err(TiffParseError::Structure(format!(
                "{context} maps a plane to IFD {ifd_index}, which already holds another plane"
            )));
        }
        ifd_usage.next = ifd_usage.next.max(ifd_index + 1);
    }
    let planes = plane_indices
        .iter()
        .map(|&ifd_index| top_ifds[ifd_index])
        .collect::<Vec<_>>();

    let bits = sample_type.byte_size() as u64 * 8;
    for (plane, &ifd_id) in planes.iter().enumerate() {
        let stored_samples = container
            .get_u32(ifd_id, tags::SAMPLES_PER_PIXEL)
            .unwrap_or(1);
        let stored_bits = container
            .get_u64_array(ifd_id, tags::BITS_PER_SAMPLE)
            .ok()
            .filter(|values| values.iter().all(|&value| Some(&value) == values.first()))
            .and_then(|values| values.first().copied());
        if stored_samples != samples_per_pixel || stored_bits != Some(bits) {
            return Err(TiffParseError::Structure(format!(
                "{context} plane {plane} stores {stored_samples} samples of {} bits, \
                 but OME-XML declares {samples_per_pixel} {} samples",
                stored_bits.map_or_else(|| "mixed or unknown".to_owned(), |b| b.to_string()),
                pixels.pixel_type
            )));
        }
    }

    Ok(MappedImage {
        index,
        image,
        shape,
        sample_type,
        samples_per_pixel,
        planes,
    })
}

/// Resolve each linear plane index to a top-level IFD index.
fn map_planes(
    context: &str,
    pixels: &OmePixels,
    shape: PlaneShape,
    ifd_count: usize,
    next_ifd: usize,
) -> Result<Vec<usize>, TiffParseError> {
    let plane_count = shape.plane_count();
    if plane_count > ifd_count as u64 {
        return Err(TiffParseError::Structure(format!(
            "{context} declares {plane_count} planes but the file has {ifd_count} IFDs"
        )));
    }
    let mut planes = vec![None; plane_count as usize];

    if pixels.tiff_data.is_empty() {
        // Without TiffData, planes follow the previous image's IFDs in order.
        for (plane, slot) in planes.iter_mut().enumerate() {
            *slot = Some(next_ifd + plane);
        }
    }
    let mut mapped_total = 0u64;
    for tiff_data in &pixels.tiff_data {
        let start = shape
            .index(tiff_data.first_z, tiff_data.first_c, tiff_data.first_t)
            .ok_or_else(|| {
                TiffParseError::Structure(format!(
                    "{context} TiffData starts at (z={}, c={}, t={}), outside the image",
                    tiff_data.first_z, tiff_data.first_c, tiff_data.first_t
                ))
            })?;
        let first_ifd = tiff_data.ifd.unwrap_or(0);
        // Per the OME-TIFF spec, PlaneCount defaults to 1 when IFD is given and
        // to every remaining plane otherwise.
        let count = tiff_data.plane_count.unwrap_or(if tiff_data.ifd.is_some() {
            1
        } else {
            plane_count - start
        });
        mapped_total = mapped_total.saturating_add(count);
        if count > plane_count - start || mapped_total > plane_count {
            return Err(TiffParseError::Structure(format!(
                "{context} TiffData elements map more than the image's {plane_count} planes"
            )));
        }
        for offset in 0..count {
            let ifd_index = first_ifd
                .checked_add(offset)
                .and_then(|value| usize::try_from(value).ok())
                .filter(|&value| value < ifd_count)
                .ok_or_else(|| {
                    TiffParseError::Structure(format!(
                        "{context} TiffData references IFD {} but the file has {ifd_count} IFDs",
                        first_ifd.saturating_add(offset)
                    ))
                })?;
            let slot = &mut planes[(start + offset) as usize];
            if slot.is_some_and(|existing| existing != ifd_index) {
                return Err(TiffParseError::Structure(format!(
                    "{context} plane {} is mapped to two different IFDs",
                    start + offset
                )));
            }
            *slot = Some(ifd_index);
        }
    }

    planes
        .into_iter()
        .enumerate()
        .map(|(plane, slot)| {
            slot.filter(|&ifd_index| ifd_index < ifd_count)
                .ok_or_else(|| {
                    let (z, c, t) = shape.coords(plane as u64);
                    TiffParseError::Structure(format!(
                        "{context} plane (z={z}, c={c}, t={t}) has no IFD in this file"
                    ))
                })
        })
        .collect()
}

fn build_series(
    container: &TiffContainer,
    scene: usize,
    image: &MappedImage<'_>,
    tile_sources: &mut HashMap<TileSourceKey, TileSource>,
) -> Result<BuiltSeries, TiffParseError> {
    let context = format!("OME-TIFF Image {}", image.index);
    let pyramid_depth = |ifd_id: IfdId| 1 + container.sub_ifds(ifd_id).len();
    let depth = image
        .planes
        .iter()
        .copied()
        .map(pyramid_depth)
        .min()
        .unwrap_or(1);
    let deepest = image
        .planes
        .iter()
        .copied()
        .map(pyramid_depth)
        .max()
        .unwrap_or(1);
    if deepest != depth {
        warn!(
            image = image.index,
            depth,
            deepest,
            "OME-TIFF planes have unequal SubIFD pyramids; exposing only the shared levels"
        );
    }
    let level_ifd = |plane: IfdId, level: usize| {
        if level == 0 {
            plane
        } else {
            container.sub_ifds(plane)[level - 1]
        }
    };

    let plane0 = image.planes[0];
    let (base_width, base_height) = ifd_dimensions(container, plane0)?;
    let pixels = &image.image.pixels;
    if (base_width, base_height) != (pixels.size_x, pixels.size_y) {
        return Err(TiffParseError::Structure(format!(
            "{context} is {}x{} in OME-XML but {base_width}x{base_height} in its first IFD",
            pixels.size_x, pixels.size_y
        )));
    }

    let mut levels = Vec::with_capacity(depth);
    let mut plane0_levels = Vec::with_capacity(depth);
    let mut ifds = Vec::with_capacity(depth * image.planes.len());
    let mut previous = (base_width, base_height);
    for level in 0..depth {
        let level0_ifd = level_ifd(plane0, level);
        let geometry = tiled_geometry(container, level0_ifd).ok_or_else(|| {
            TiffParseError::Structure(format!(
                "{context} level {level} is not tiled; only tiled OME-TIFF planes are supported"
            ))
        })?;
        let (width, height, tile_width, tile_height) = geometry;
        if width > previous.0 || height > previous.1 {
            return Err(TiffParseError::Structure(format!(
                "{context} level {level} ({width}x{height}) is larger than the level above it"
            )));
        }
        previous = (width, height);
        let downsample = if level == 0 {
            1.0
        } else {
            (base_width as f64 / width as f64 + base_height as f64 / height as f64) / 2.0
        };
        levels.push(regular_tiff_level(
            "OME-TIFF",
            width,
            height,
            tile_width,
            tile_height,
            downsample,
        )?);
        plane0_levels.push(level0_ifd);

        for (plane_index, &plane) in image.planes.iter().enumerate() {
            let ifd_id = level_ifd(plane, level);
            if tiled_geometry(container, ifd_id) != Some(geometry) {
                return Err(TiffParseError::Structure(format!(
                    "{context} plane {plane_index} level {level} differs in size or tiling from plane 0"
                )));
            }
            let (z, c, t) = image.shape.coords(plane_index as u64);
            tile_sources.insert(
                TileSourceKey {
                    scene,
                    series: 0,
                    level: level as u32,
                    z,
                    c,
                    t,
                },
                tiled_source(container, ifd_id),
            );
            ifds.push(ifd_id);
        }
    }

    // Interleaved RGB is brightfield and, like other formats, carries no channel list.
    let channels = if image.samples_per_pixel == 3 && image.shape.c == 1 {
        Vec::new()
    } else {
        (0..image.shape.c)
            .map(|c| {
                let info = pixels
                    .channels
                    .get(c as usize)
                    .map(|channel| channel.info.clone())
                    .unwrap_or_default();
                if info.name.is_some() {
                    info
                } else {
                    info.with_name(format!("Channel {c}"))
                }
            })
            .collect()
    };

    Ok(BuiltSeries {
        series: Series::new(
            "ser0",
            AxesShape::new(image.shape.z, image.shape.c, image.shape.t),
            levels,
            image.sample_type,
            channels,
        ),
        plane0_levels,
        ifds,
    })
}

/// Associated-image name for a single-plane 8-bit image named after one,
/// such as Bio-Formats' "label image". The first image is always a scene.
fn associated_name(image: &MappedImage<'_>, image_count: usize) -> Option<&'static str> {
    if image.index == 0
        || image_count < 2
        || image.planes.len() != 1
        || image.sample_type != SampleType::Uint8
    {
        return None;
    }
    let name = image.image.name.as_deref()?.trim().to_ascii_lowercase();
    match name.strip_suffix(" image").unwrap_or(&name) {
        "label" => Some("label"),
        "macro" | "overview" => Some("macro"),
        "thumbnail" => Some("thumbnail"),
        _ => None,
    }
}

fn unique_name(existing: &HashMap<String, AssociatedImage>, base: &str) -> String {
    if !existing.contains_key(base) {
        return base.into();
    }
    (1..)
        .map(|index| format!("{base}-{index}"))
        .find(|candidate| !existing.contains_key(candidate))
        .unwrap_or_else(|| unreachable!("unbounded suffix search must eventually return"))
}

fn ome_properties(
    document: &OmeDocument,
    description: &str,
    main: &[MappedImage<'_>],
) -> Properties {
    let mut properties = Properties::new();
    properties.insert("openslide.vendor", VENDOR);
    properties.insert("openslide.comment", description);
    if let Some(first) = main.first() {
        let pixels = &first.image.pixels;
        if let Some(mpp) = pixels
            .physical_size_x
            .as_ref()
            .and_then(PhysicalSize::microns)
        {
            properties.insert("openslide.mpp-x", mpp.to_string());
        }
        if let Some(mpp) = pixels
            .physical_size_y
            .as_ref()
            .and_then(PhysicalSize::microns)
        {
            properties.insert("openslide.mpp-y", mpp.to_string());
        }
        if let Some(power) = document.objective_power(first.image) {
            properties.insert("openslide.objective-power", power);
        }
    }
    if let Some(creator) = &document.creator {
        properties.insert("ome.Creator", creator.as_str());
    }
    if let Some(uuid) = &document.uuid {
        properties.insert("ome.UUID", uuid.as_str());
    }
    for (index, image) in document.images.iter().enumerate() {
        let prefix = format!("ome.image[{index}]");
        for (key, value) in &image.image_attributes {
            properties.insert(format!("{prefix}.{key}"), value.as_str());
        }
        for (key, value) in &image.pixels_attributes {
            properties.insert(format!("{prefix}.Pixels.{key}"), value.as_str());
        }
        for (channel, attributes) in image.channel_attributes.iter().enumerate() {
            for (key, value) in attributes {
                properties.insert(
                    format!("{prefix}.Pixels.Channel[{channel}].{key}"),
                    value.as_str(),
                );
            }
        }
    }
    properties
}

fn ifd_dimensions(container: &TiffContainer, ifd_id: IfdId) -> Result<(u64, u64), TiffParseError> {
    Ok((
        container.get_u64(ifd_id, tags::IMAGE_WIDTH)?,
        container.get_u64(ifd_id, tags::IMAGE_LENGTH)?,
    ))
}

/// `(width, height, tile_width, tile_height)` of a tiled IFD.
fn tiled_geometry(container: &TiffContainer, ifd_id: IfdId) -> Option<(u64, u64, u32, u32)> {
    let (width, height) = ifd_dimensions(container, ifd_id).ok()?;
    Some((
        width,
        height,
        container.get_u32(ifd_id, tags::TILE_WIDTH).ok()?,
        container.get_u32(ifd_id, tags::TILE_LENGTH).ok()?,
    ))
}

fn jpeg_tables(container: &TiffContainer, ifd_id: IfdId) -> Option<Vec<u8>> {
    container
        .get_bytes(ifd_id, tags::JPEG_TABLES)
        .ok()
        .map(<[u8]>::to_vec)
}

fn compression(container: &TiffContainer, ifd_id: IfdId) -> Compression {
    compression_from_tag(container.get_u32(ifd_id, tags::COMPRESSION).unwrap_or(1))
}

fn tiled_source(container: &TiffContainer, ifd_id: IfdId) -> TileSource {
    TileSource::TiledIfd {
        ifd_id,
        jpeg_tables: jpeg_tables(container, ifd_id),
        compression: compression(container, ifd_id),
    }
}

fn associated_source(container: &TiffContainer, ifd_id: IfdId) -> TileSource {
    if tiled_geometry(container, ifd_id).is_some() {
        return tiled_source(container, ifd_id);
    }
    TileSource::Stripped {
        ifd_id,
        jpeg_tables: jpeg_tables(container, ifd_id),
        compression: compression(container, ifd_id),
        strip_offsets: container
            .get_u64_array(ifd_id, tags::STRIP_OFFSETS)
            .map(<[u64]>::to_vec)
            .unwrap_or_default(),
        strip_byte_counts: container
            .get_u64_array(ifd_id, tags::STRIP_BYTE_COUNTS)
            .map(<[u64]>::to_vec)
            .unwrap_or_default(),
    }
}
