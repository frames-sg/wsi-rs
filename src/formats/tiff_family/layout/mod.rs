//! Layer 2 types for TIFF-family layout interpretation.
//!
//! A `TiffLayoutInterpreter` maps raw IFDs from a `TiffContainer` into the
//! normalized `Dataset` model plus a `DatasetLayout` that records how to
//! access each level's pixel data at decode time.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::core::hash::Quickhash1;
use crate::core::types::{
    AssociatedImage, AxesShape, Compression, Dataset, DatasetId, Level, SampleType, Scene, Series,
};
use crate::error::WsiError;
use crate::formats::tiff_family::container::{tags, TiffContainer};
use crate::formats::tiff_family::error::{IfdId, TiffParseError};
use crate::formats::tiff_family::icc::attach_source_icc_profile;
use crate::properties::Properties;

pub(crate) mod aperio;
pub(crate) mod argos;
pub(crate) mod generic;
pub(crate) mod huron;
pub(crate) mod leica;
pub(crate) mod ndpi;
pub(crate) mod philips;
pub(crate) mod trestle;
pub(crate) mod ventana;

const QUICKHASH_MAX_LEVEL_BYTES: u64 = 5 << 20;
const TAG_DOCUMENT_NAME: u16 = 269;
const TAG_MAKE: u16 = 271;
const TAG_MODEL: u16 = 272;
const TAG_SOFTWARE: u16 = 305;
const TAG_DATETIME: u16 = 306;
const TAG_ARTIST: u16 = 315;
const TAG_HOST_COMPUTER: u16 = 316;
const TAG_COPYRIGHT: u16 = 33432;

fn compression_from_tag(val: u32) -> Compression {
    match val {
        1 => Compression::None,
        5 => Compression::Lzw,
        8 | 32946 => Compression::Deflate,
        6 | 7 => Compression::Jpeg,
        50000 => Compression::Zstd,
        33003 | 33005 => Compression::Jp2kYcbcr,
        33004 => Compression::Jp2kRgb,
        _ => Compression::Other(val as u16),
    }
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

fn regular_tiff_level(
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

fn add_tiff_resolution_properties(
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

fn compute_tiff_dataset_id_and_record_quickhash(
    container: &TiffContainer,
    lowest_resolution_ifd: IfdId,
    property_ifd: IfdId,
    properties: &mut Properties,
) -> Result<DatasetId, TiffParseError> {
    let identity = compute_tiff_dataset_identity(container, lowest_resolution_ifd, property_ifd)?;
    if let Some(quickhash1) = identity.quickhash1.as_deref() {
        properties.insert("openslide.quickhash-1", quickhash1);
    }
    Ok(identity.dataset_id)
}

#[allow(clippy::too_many_arguments)]
fn finish_single_scene_uint8_tiff_layout<I>(
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

pub(crate) struct DatasetIdentity {
    pub dataset_id: DatasetId,
    pub quickhash1: Option<String>,
}

// ── Interpreter trait ────────────────────────────────────────────────────────

/// Trait for vendor-specific TIFF layout interpretation.
/// Implementations map raw IFDs into the normalized Dataset model.
pub(crate) trait TiffLayoutInterpreter: Send + Sync {
    /// Returns true if this interpreter can handle the given container.
    /// Must be cheap — only inspect tags already loaded into `container`.
    fn detect(&self, container: &TiffContainer) -> bool;

    /// Interpret the container, producing a `DatasetLayout`.
    /// Returns `TiffParseError`; callers convert to `WsiError` at the
    /// `TiffFamilyBackend` boundary.
    fn interpret(&self, container: &TiffContainer) -> Result<DatasetLayout, TiffParseError>;

    /// Short vendor identifier for probe results (e.g., "aperio", "leica").
    fn vendor_name(&self) -> &'static str;
}

// ── Output types ─────────────────────────────────────────────────────────────

/// Output of a layout interpreter. Bundles the normalized metadata `Dataset`
/// with the two maps the pixel reader needs for dispatch.
#[derive(Debug)]
pub(crate) struct DatasetLayout {
    /// Normalized metadata tree for this file.
    pub dataset: Dataset,

    /// Maps each (scene, series, level, z, c, t) plane to its pixel source.
    /// The pixel reader uses this to dispatch `TileRequest`s to the correct
    /// IFD or NDPI JPEG path.
    pub tile_sources: HashMap<TileSourceKey, TileSource>,

    /// Maps associated image names ("macro", "label", "thumbnail") to their
    /// pixel sources. `Dataset::associated_images` stores only metadata
    /// (dimensions, sample_type); this map provides the IFD/strip reference
    /// needed to decode.
    pub associated_sources: HashMap<String, TileSource>,
}

/// Composite key for tile source lookup. Identifies a plane, not a tile.
/// The pixel reader computes tile addressing within the plane.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub(crate) struct TileSourceKey {
    pub scene: usize,
    pub series: usize,
    pub level: u32,
    pub z: u32,
    pub c: u32,
    pub t: u32,
}

/// Describes how to access a level's or associated image's pixel data.
#[derive(Debug, Clone)]
pub(crate) enum TileSource {
    /// Standard tiled TIFF IFD (Aperio SVS, OME-TIFF, etc.).
    TiledIfd {
        ifd_id: IfdId,
        /// JPEG tables from JPEGTables tag (tag 347), if present.
        jpeg_tables: Option<Vec<u8>>,
        compression: Compression,
    },

    /// NDPI giant JPEG with MCU-boundary extraction (fast path).
    ///
    /// `tiles_across` and `tiles_down` are derived from
    /// `ceil(image_width / virtual_tile_width)` and
    /// `ceil(image_height / virtual_tile_height)` at layout time.
    /// The pixel reader uses row-major indexing:
    ///   `idx = row * tiles_across + col`
    ///
    /// `strip_offset` and `strip_byte_count` are needed to compute the
    /// end boundary for the last tile:
    ///   `end = strip_offset + strip_byte_count` when
    ///   `idx + 1 == mcu_starts.len()`
    NdpiJpeg {
        ifd_id: IfdId,
        /// JPEG header bytes (SOI through end of SOS segment).
        jpeg_header: Vec<u8>,
        /// Tag number of the MCU-starts array (NDPI tag 65426).
        /// Resolved lazily by the pixel reader on first tile access.
        mcu_starts_tag: u16,
        /// Number of virtual tiles per row.
        tiles_across: u32,
        /// Number of virtual tile rows.
        tiles_down: u32,
        /// Restart interval in MCUs (from DRI marker).
        restart_interval: u16,
        /// Strip byte offset — used to compute last-tile end boundary.
        strip_offset: u64,
        /// Strip byte count — used to compute last-tile end boundary.
        strip_byte_count: u64,
    },

    /// NDPI level without restart markers — full decode required.
    ///
    /// The pixel reader decodes the entire JPEG once, caches it in
    /// `FullDecodeCache`, and extracts virtual tile regions on demand.
    NdpiFullDecode {
        ifd_id: IfdId,
        /// JPEG header bytes (SOI through end of SOS segment).
        jpeg_header: Vec<u8>,
        strip_offset: u64,
        strip_byte_count: u64,
    },

    /// Synthetic NDPI power-of-two level derived from a higher-resolution level.
    ///
    /// The compatibility model exposes a complete power-of-two NDPI pyramid even when the
    /// underlying file only stores sparse physical resolutions. `base_level`
    /// points at the nearest higher-resolution level already present in the
    /// public pyramid, and `factor` is the power-of-two reduction relative to
    /// that level.
    SyntheticDownsample { base_level: u32, factor: u32 },

    /// Stripped TIFF (associated images, older formats).
    Stripped {
        ifd_id: IfdId,
        /// JPEG tables from JPEGTables tag (tag 347), if present.
        jpeg_tables: Option<Vec<u8>>,
        compression: Compression,
        strip_offsets: Vec<u64>,
        strip_byte_counts: Vec<u64>,
    },

    /// Generic strip-based TIFF exposed as a synthetic regular tile grid.
    StrippedLevel {
        ifd_id: IfdId,
        compression: Compression,
        strip_offsets: Vec<u64>,
        strip_byte_counts: Vec<u64>,
    },

    /// Associated image stored as an external JPEG sidecar file.
    ExternalJpeg { path: PathBuf },
}

pub(crate) fn compute_tiff_dataset_identity(
    container: &TiffContainer,
    lowest_resolution_ifd: IfdId,
    property_ifd: IfdId,
) -> Result<DatasetIdentity, TiffParseError> {
    compute_tiff_dataset_identity_with_extra_strings(
        container,
        lowest_resolution_ifd,
        property_ifd,
        &[],
    )
}

pub(crate) fn compute_tiff_dataset_identity_with_extra_strings(
    container: &TiffContainer,
    lowest_resolution_ifd: IfdId,
    property_ifd: IfdId,
    extra_strings: &[&str],
) -> Result<DatasetIdentity, TiffParseError> {
    let quickhash1 = compute_tiff_quickhash(
        container,
        lowest_resolution_ifd,
        property_ifd,
        extra_strings,
    )?;
    let dataset_id = match quickhash1.as_deref() {
        Some(hash) => dataset_id_from_hex(hash)?,
        None => fallback_dataset_id(
            container,
            lowest_resolution_ifd,
            property_ifd,
            extra_strings,
        )?,
    };
    Ok(DatasetIdentity {
        dataset_id,
        quickhash1,
    })
}

fn compute_tiff_quickhash(
    container: &TiffContainer,
    lowest_resolution_ifd: IfdId,
    property_ifd: IfdId,
    extra_strings: &[&str],
) -> Result<Option<String>, TiffParseError> {
    let mut hash = Quickhash1::new();

    if !hash_tiff_level(&mut hash, container, lowest_resolution_ifd)? {
        return Ok(None);
    }

    hash_tiff_string_properties(&mut hash, container, property_ifd);
    for value in extra_strings {
        hash.hash_string(value);
    }
    Ok(hash.finish())
}

fn hash_tiff_level(
    hash: &mut Quickhash1,
    container: &TiffContainer,
    ifd_id: IfdId,
) -> Result<bool, TiffParseError> {
    let ranges = tiff_data_ranges(container, ifd_id)?;
    let total_bytes: u64 = ranges.iter().map(|(_, len)| *len).sum();
    if total_bytes > QUICKHASH_MAX_LEVEL_BYTES {
        hash.disable();
        return Ok(false);
    }

    for (offset, len) in ranges {
        if len == 0 {
            continue;
        }
        hash.hash_file_part(container.path(), offset, Some(len))
            .map_err(wsi_to_tiff_error)?;
    }

    Ok(true)
}

fn tiff_data_ranges(
    container: &TiffContainer,
    ifd_id: IfdId,
) -> Result<Vec<(u64, u64)>, TiffParseError> {
    let ifd = container.ifd_by_id(ifd_id)?;
    let (offset_tag, length_tag) = if ifd.tags.contains_key(&tags::TILE_WIDTH) {
        (tags::TILE_OFFSETS, tags::TILE_BYTE_COUNTS)
    } else {
        (tags::STRIP_OFFSETS, tags::STRIP_BYTE_COUNTS)
    };
    let offsets = match container.get_u64_array(ifd_id, offset_tag) {
        Ok(values) => values,
        Err(TiffParseError::TagNotFound { .. }) => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let lengths = match container.get_u64_array(ifd_id, length_tag) {
        Ok(values) => values,
        Err(TiffParseError::TagNotFound { .. }) => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    if offsets.len() != lengths.len() {
        return Err(TiffParseError::Structure(format!(
            "IFD {} has {} offsets but {} byte counts",
            ifd_id,
            offsets.len(),
            lengths.len()
        )));
    }

    Ok(offsets
        .iter()
        .copied()
        .zip(lengths.iter().copied())
        .collect())
}

fn hash_tiff_string_properties(hash: &mut Quickhash1, container: &TiffContainer, ifd_id: IfdId) {
    const STRING_PROPS: [(&str, u16); 8] = [
        ("tiff.ImageDescription", tags::IMAGE_DESCRIPTION),
        ("tiff.Make", TAG_MAKE),
        ("tiff.Model", TAG_MODEL),
        ("tiff.Software", TAG_SOFTWARE),
        ("tiff.DateTime", TAG_DATETIME),
        ("tiff.Artist", TAG_ARTIST),
        ("tiff.HostComputer", TAG_HOST_COMPUTER),
        ("tiff.Copyright", TAG_COPYRIGHT),
    ];

    for (name, tag) in STRING_PROPS {
        hash_named_tiff_string_property(hash, container, ifd_id, name, tag);
    }
    hash_named_tiff_string_property(
        hash,
        container,
        ifd_id,
        "tiff.DocumentName",
        TAG_DOCUMENT_NAME,
    );
}

fn hash_named_tiff_string_property(
    hash: &mut Quickhash1,
    container: &TiffContainer,
    ifd_id: IfdId,
    name: &str,
    tag: u16,
) {
    hash.hash_string(name);
    hash.hash_string(container.get_string(ifd_id, tag).unwrap_or(""));
}

fn dataset_id_from_hex(hex: &str) -> Result<DatasetId, TiffParseError> {
    if hex.len() < 32 {
        return Err(TiffParseError::Structure(format!(
            "quickhash too short: expected at least 32 hex chars, got {}",
            hex.len()
        )));
    }

    let prefix = &hex[..32];
    let value = u128::from_str_radix(prefix, 16).map_err(|err| {
        TiffParseError::Structure(format!("invalid quickhash hex prefix '{prefix}': {err}"))
    })?;
    Ok(DatasetId::new(value))
}

fn fallback_dataset_id(
    container: &TiffContainer,
    lowest_resolution_ifd: IfdId,
    property_ifd: IfdId,
    extra_strings: &[&str],
) -> Result<DatasetId, TiffParseError> {
    let mut hash = Quickhash1::new();
    hash.update(&container.ifd_count().to_le_bytes());
    hash.update(&(container.top_ifds().len() as u64).to_le_bytes());
    hash.update(&[match container.endian() {
        crate::formats::tiff_family::container::Endian::Little => 0,
        crate::formats::tiff_family::container::Endian::Big => 1,
    }]);
    hash.update(&[u8::from(container.is_bigtiff())]);

    let ifd = container.ifd_by_id(lowest_resolution_ifd)?;
    hash.update(&ifd.offset.to_le_bytes());
    for (offset, len) in tiff_data_ranges(container, lowest_resolution_ifd)? {
        hash.update(&offset.to_le_bytes());
        hash.update(&len.to_le_bytes());
    }
    hash_tiff_string_properties(&mut hash, container, property_ifd);
    for value in extra_strings {
        hash.hash_string(value);
    }

    let hex = hash.finish().ok_or_else(|| {
        TiffParseError::Structure("fallback dataset hash unexpectedly disabled".into())
    })?;
    dataset_id_from_hex(&hex)
}

fn wsi_to_tiff_error(err: WsiError) -> TiffParseError {
    match err {
        WsiError::Io(source) => source.into(),
        WsiError::IoWithPath { source, path } => TiffParseError::Io {
            kind: source.kind(),
            source,
            path: Some(std::sync::Arc::new(path)),
        },
        other => TiffParseError::Structure(other.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
