use std::{error::Error, fmt};

use super::*;

// ── Dataset hierarchy ──────────────────────────────────────────────

/// A whole-slide image file (or set of files for DICOM).
#[derive(Debug)]
#[non_exhaustive]
pub struct Dataset {
    pub id: DatasetId,
    pub scenes: Vec<Scene>,
    pub associated_images: HashMap<String, AssociatedImage>,
    pub properties: Properties,
    pub icc_profiles: HashMap<IccProfileKey, Vec<u8>>,
    /// Source ICC profiles with provenance and optional source-specific
    /// refinements.
    pub source_icc_profiles: Vec<SourceIccProfile>,
}

impl Dataset {
    pub fn new(id: DatasetId, scenes: Vec<Scene>) -> Self {
        Self {
            id,
            scenes,
            associated_images: HashMap::new(),
            properties: Properties::new(),
            icc_profiles: HashMap::new(),
            source_icc_profiles: Vec::new(),
        }
    }

    pub fn with_associated_images(
        mut self,
        associated_images: HashMap<String, AssociatedImage>,
    ) -> Self {
        self.associated_images = associated_images;
        self
    }

    pub fn with_properties(mut self, properties: Properties) -> Self {
        self.properties = properties;
        self
    }

    pub fn with_icc_profiles(mut self, icc_profiles: HashMap<IccProfileKey, Vec<u8>>) -> Self {
        self.icc_profiles = icc_profiles;
        self
    }

    /// Adds a source ICC profile and keeps legacy ICC metadata in sync.
    ///
    /// Profiles with no `optical_path` and no `channel` are unqualified
    /// scene/series profiles, so their bytes are also written to
    /// [`Dataset::icc_profiles`]. Qualified profiles are appended only to
    /// [`Dataset::source_icc_profiles`].
    ///
    /// If an unqualified profile would overwrite different existing legacy
    /// bytes for the same typed [`IccProfileKey`], this returns
    /// [`SourceIccProfileConflict`] and leaves both ICC collections unchanged.
    /// Identical unqualified bytes are accepted because they do not create
    /// legacy-map ambiguity.
    pub fn push_source_icc_profile(
        &mut self,
        profile: SourceIccProfile,
    ) -> Result<(), SourceIccProfileConflict> {
        if profile.key.optical_path.is_none() && profile.key.channel.is_none() {
            let legacy_key = IccProfileKey::new(profile.key.scene, profile.key.series);
            if let Some(existing_bytes) = self.icc_profiles.get(&legacy_key) {
                if existing_bytes != &profile.bytes {
                    return Err(SourceIccProfileConflict {
                        scene: profile.key.scene,
                        series: profile.key.series,
                    });
                }
            }
            self.icc_profiles.insert(legacy_key, profile.bytes.clone());
        }
        self.source_icc_profiles.push(profile);
        Ok(())
    }

    /// Iterates over source ICC profiles for a normalized scene/series pair.
    ///
    /// Matching is limited to the `scene` and `series` fields. Optional
    /// `optical_path` and `channel` refinements remain available on each
    /// returned [`SourceIccProfile`].
    pub fn source_icc_profiles_for_series(
        &self,
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
    ) -> impl Iterator<Item = &SourceIccProfile> {
        let scene = scene.into();
        let series = series.into();
        self.source_icc_profiles
            .iter()
            .filter(move |profile| profile.key.scene == scene && profile.key.series == series)
    }
}

/// Identifies the scene and series an ICC profile applies to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct IccProfileKey {
    pub scene: SceneId,
    pub series: SeriesId,
}

impl IccProfileKey {
    pub const fn new(scene: SceneId, series: SeriesId) -> Self {
        Self { scene, series }
    }
}

/// Normalized dataset location for an ICC profile found in source metadata.
///
/// `scene` and `series` identify the normalized dataset hierarchy.
/// `optical_path` and `channel` are optional numeric refinements for source
/// profiles that apply below the scene/series level. DICOM optical path string
/// identifiers are stored in [`IccProfileProvenance::DicomOpticalPath`], not in
/// this key.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct SourceIccProfileKey {
    /// Normalized scene index.
    pub scene: SceneId,
    /// Normalized series index within `scene`.
    pub series: SeriesId,
    /// Optional zero-based normalized optical path refinement.
    pub optical_path: Option<usize>,
    /// Optional zero-based normalized channel refinement.
    pub channel: Option<usize>,
}

impl SourceIccProfileKey {
    #[must_use]
    pub const fn new(scene: SceneId, series: SeriesId) -> Self {
        Self {
            scene,
            series,
            optical_path: None,
            channel: None,
        }
    }

    #[must_use]
    pub const fn with_optical_path(mut self, optical_path: usize) -> Self {
        self.optical_path = Some(optical_path);
        self
    }

    #[must_use]
    pub const fn with_channel(mut self, channel: usize) -> Self {
        self.channel = Some(channel);
        self
    }
}

/// Source ICC profile bytes plus the normalized key and extraction provenance.
///
/// Use [`SourceIccProfileKey`] for dataset indices and numeric refinements.
/// Source string identifiers, such as DICOM optical path identifiers, are kept
/// in [`IccProfileProvenance`].
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub struct SourceIccProfile {
    /// Normalized dataset key for this ICC profile.
    pub key: SourceIccProfileKey,
    /// Raw ICC profile bytes from the source metadata.
    pub bytes: Vec<u8>,
    /// Where this profile was found.
    pub provenance: IccProfileProvenance,
}

impl SourceIccProfile {
    #[must_use]
    pub fn new(key: SourceIccProfileKey, bytes: Vec<u8>, provenance: IccProfileProvenance) -> Self {
        Self {
            key,
            bytes,
            provenance,
        }
    }
}

/// Provenance for ICC profile bytes extracted from source metadata.
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum IccProfileProvenance {
    /// ICC profile found in a TIFF tag.
    TiffTag {
        /// Normalized TIFF IFD identifier used by the TIFF parser.
        ifd_id: u64,
        /// TIFF tag number that carried the ICC bytes.
        tag: u16,
    },
    /// ICC profile found in DICOM optical path metadata.
    DicomOpticalPath {
        /// SOP Instance UID for the DICOM instance that carried the metadata.
        sop_instance_uid: String,
        /// Source DICOM optical path string identifier, when present.
        optical_path_identifier: Option<String>,
    },
    /// ICC profile reported by reader-level metadata outside a typed container.
    ReaderMetadata {
        /// Reader-specific source description.
        source: String,
    },
}

/// Conflict returned when a source ICC profile would make the legacy map
/// ambiguous.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub struct SourceIccProfileConflict {
    /// Normalized scene index.
    pub scene: SceneId,
    /// Normalized series index within `scene`.
    pub series: SeriesId,
}

impl fmt::Display for SourceIccProfileConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "conflicting source ICC profiles for scene {} series {}",
            self.scene.get(),
            self.series.get()
        )
    }
}

impl Error for SourceIccProfileConflict {}

/// A distinct scan region within a dataset.
#[derive(Debug)]
#[non_exhaustive]
pub struct Scene {
    pub id: String,
    pub name: Option<String>,
    pub series: Vec<Series>,
}

impl Scene {
    pub fn new(id: impl Into<String>, series: Vec<Series>) -> Self {
        Self {
            id: id.into(),
            name: None,
            series,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// A coherent image pyramid sharing the same axes and sample type.
#[derive(Debug)]
#[non_exhaustive]
pub struct Series {
    pub id: String,
    pub axes: AxesShape,
    pub levels: Vec<Level>,
    pub sample_type: SampleType,
    pub channels: Vec<ChannelInfo>,
}

impl Series {
    pub fn new(
        id: impl Into<String>,
        axes: AxesShape,
        levels: Vec<Level>,
        sample_type: SampleType,
        channels: Vec<ChannelInfo>,
    ) -> Self {
        Self {
            id: id.into(),
            axes,
            levels,
            sample_type,
            channels,
        }
    }
}

/// Axis extents beyond x/y. Default is 2D (z=1, c=1, t=1).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub struct AxesShape {
    pub z: u32,
    pub c: u32,
    pub t: u32,
}

impl AxesShape {
    pub const fn new(z: u32, c: u32, t: u32) -> Self {
        Self { z, c, t }
    }
}

impl Default for AxesShape {
    fn default() -> Self {
        Self { z: 1, c: 1, t: 1 }
    }
}

/// One resolution level in a pyramid.
#[derive(Debug)]
#[non_exhaustive]
pub struct Level {
    pub dimensions: (u64, u64),
    pub downsample: f64,
    pub tile_layout: TileLayout,
}

impl Level {
    pub fn new(dimensions: (u64, u64), downsample: f64, tile_layout: TileLayout) -> Self {
        Self {
            dimensions,
            downsample,
            tile_layout,
        }
    }
}

/// Whether a pyramid level is backed by source pixels or generated by the reader.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum LevelSourceKind {
    /// The level has pixel data backed directly by the source dataset.
    Physical,
    /// The level is synthesized by downsampling another level.
    SyntheticDownsample,
}
// ── Channel / Associated image metadata ────────────────────────────

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ChannelInfo {
    pub name: Option<String>,
    pub color: Option<[u8; 3]>,
    pub excitation_nm: Option<f64>,
    pub emission_nm: Option<f64>,
}

impl ChannelInfo {
    pub fn new() -> Self {
        Self {
            name: None,
            color: None,
            excitation_nm: None,
            emission_nm: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_color(mut self, color: [u8; 3]) -> Self {
        self.color = Some(color);
        self
    }

    pub fn with_excitation_nm(mut self, excitation_nm: f64) -> Self {
        self.excitation_nm = Some(excitation_nm);
        self
    }

    pub fn with_emission_nm(mut self, emission_nm: f64) -> Self {
        self.emission_nm = Some(emission_nm);
        self
    }
}

impl Default for ChannelInfo {
    fn default() -> Self {
        Self::new()
    }
}

/// Metadata for an associated image (label, macro, thumbnail).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AssociatedImage {
    pub dimensions: (u32, u32),
    pub sample_type: SampleType,
    pub channels: u16,
}

impl AssociatedImage {
    pub const fn new(dimensions: (u32, u32), sample_type: SampleType, channels: u16) -> Self {
        Self {
            dimensions,
            sample_type,
            channels,
        }
    }
}

// ── Identity / Compression ─────────────────────────────────────────

/// Unique identity for cache keying. 128-bit to avoid truncation collisions.
#[derive(Clone, Copy, Hash, Eq, PartialEq, Debug)]
#[non_exhaustive]
pub struct DatasetId(pub(crate) u128);

impl DatasetId {
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u128 {
        self.0
    }
}

/// Stable index into `Dataset::scenes`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct SceneId(pub(crate) usize);

impl SceneId {
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

impl From<usize> for SceneId {
    fn from(index: usize) -> Self {
        Self::new(index)
    }
}

/// Stable index into `Scene::series`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct SeriesId(pub(crate) usize);

impl SeriesId {
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

impl From<usize> for SeriesId {
    fn from(index: usize) -> Self {
        Self::new(index)
    }
}

/// Stable index into `Series::levels`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct LevelIdx(pub(crate) u32);

impl LevelIdx {
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<u32> for LevelIdx {
    fn from(index: u32) -> Self {
        Self::new(index)
    }
}

/// Plane index for multi-dimensional axes (z/c/t).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default)]
#[non_exhaustive]
pub struct PlaneIdx(pub(crate) PlaneSelection);

impl PlaneIdx {
    pub const fn new(plane: PlaneSelection) -> Self {
        Self(plane)
    }

    pub const fn get(self) -> PlaneSelection {
        self.0
    }
}

impl From<PlaneSelection> for PlaneIdx {
    fn from(plane: PlaneSelection) -> Self {
        Self::new(plane)
    }
}

/// Compression codec for TIFF tile/strip data.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum Compression {
    None,
    Lzw,
    Deflate,
    Zstd,
    Jpeg,
    Jp2kYcbcr,
    Jp2kRgb,
    JpegLs,
    Other(u16),
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum TileCodecKind {
    Jpeg,
    Jp2k,
    Htj2k,
    #[default]
    Other,
}

impl TileCodecKind {
    pub fn from_compression(compression: Compression) -> Self {
        match compression {
            Compression::Jpeg => Self::Jpeg,
            Compression::Jp2kRgb | Compression::Jp2kYcbcr => Self::Jp2k,
            _ => Self::Other,
        }
    }
}

/// Photometric interpretation for an encoded tile payload.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum EncodedTilePhotometricInterpretation {
    Monochrome2,
    Rgb,
    YbrFull422,
}

/// Raw compressed tile bytes that can be copied into another container without
/// decoding pixels.
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub struct RawCompressedTile {
    compression: Compression,
    width: u32,
    height: u32,
    bits_allocated: u16,
    samples_per_pixel: u16,
    photometric_interpretation: EncodedTilePhotometricInterpretation,
    data: Vec<u8>,
}

impl RawCompressedTile {
    pub fn builder(compression: Compression) -> RawCompressedTileBuilder {
        RawCompressedTileBuilder {
            compression,
            dimensions: None,
            bits_allocated: None,
            samples_per_pixel: None,
            photometric_interpretation: None,
            data: None,
        }
    }

    pub fn compression(&self) -> Compression {
        self.compression
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn bits_allocated(&self) -> u16 {
        self.bits_allocated
    }

    pub fn samples_per_pixel(&self) -> u16 {
        self.samples_per_pixel
    }

    pub fn photometric_interpretation(&self) -> EncodedTilePhotometricInterpretation {
        self.photometric_interpretation
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn into_data(self) -> Vec<u8> {
        self.data
    }
}

/// Builder for [`RawCompressedTile`] that names payload metadata fields.
#[derive(Debug, Clone)]
#[must_use]
pub struct RawCompressedTileBuilder {
    compression: Compression,
    dimensions: Option<(u32, u32)>,
    bits_allocated: Option<u16>,
    samples_per_pixel: Option<u16>,
    photometric_interpretation: Option<EncodedTilePhotometricInterpretation>,
    data: Option<Vec<u8>>,
}

impl RawCompressedTileBuilder {
    pub fn dimensions(mut self, width: u32, height: u32) -> Self {
        self.dimensions = Some((width, height));
        self
    }

    pub fn bits_allocated(mut self, bits_allocated: u16) -> Self {
        self.bits_allocated = Some(bits_allocated);
        self
    }

    pub fn samples_per_pixel(mut self, samples_per_pixel: u16) -> Self {
        self.samples_per_pixel = Some(samples_per_pixel);
        self
    }

    pub fn photometric_interpretation(
        mut self,
        photometric_interpretation: EncodedTilePhotometricInterpretation,
    ) -> Self {
        self.photometric_interpretation = Some(photometric_interpretation);
        self
    }

    pub fn data(mut self, data: Vec<u8>) -> Self {
        self.data = Some(data);
        self
    }

    pub fn build(self) -> Result<RawCompressedTile, RawCompressedTileBuildError> {
        let (width, height) = self
            .dimensions
            .ok_or(RawCompressedTileBuildError::MissingDimensions)?;
        let bits_allocated = self
            .bits_allocated
            .ok_or(RawCompressedTileBuildError::MissingBitsAllocated)?;
        let samples_per_pixel = self
            .samples_per_pixel
            .ok_or(RawCompressedTileBuildError::MissingSamplesPerPixel)?;
        let photometric_interpretation = self
            .photometric_interpretation
            .ok_or(RawCompressedTileBuildError::MissingPhotometricInterpretation)?;
        let data = self.data.ok_or(RawCompressedTileBuildError::MissingData)?;

        if width == 0 || height == 0 {
            return Err(RawCompressedTileBuildError::InvalidDimensions);
        }
        if bits_allocated == 0 {
            return Err(RawCompressedTileBuildError::InvalidBitsAllocated);
        }
        if samples_per_pixel == 0 {
            return Err(RawCompressedTileBuildError::InvalidSamplesPerPixel);
        }
        if data.is_empty() {
            return Err(RawCompressedTileBuildError::EmptyData);
        }

        Ok(RawCompressedTile {
            compression: self.compression,
            width,
            height,
            bits_allocated,
            samples_per_pixel,
            photometric_interpretation,
            data,
        })
    }
}

/// Error returned by [`RawCompressedTileBuilder`] when required metadata is missing.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum RawCompressedTileBuildError {
    MissingDimensions,
    MissingBitsAllocated,
    MissingSamplesPerPixel,
    MissingPhotometricInterpretation,
    MissingData,
    InvalidDimensions,
    InvalidBitsAllocated,
    InvalidSamplesPerPixel,
    EmptyData,
}

impl fmt::Display for RawCompressedTileBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingDimensions => "raw compressed tile dimensions are required",
            Self::MissingBitsAllocated => "raw compressed tile bit depth is required",
            Self::MissingSamplesPerPixel => "raw compressed tile sample count is required",
            Self::MissingPhotometricInterpretation => {
                "raw compressed tile photometric interpretation is required"
            }
            Self::MissingData => "raw compressed tile payload data is required",
            Self::InvalidDimensions => "raw compressed tile dimensions must be positive",
            Self::InvalidBitsAllocated => "raw compressed tile bit depth must be positive",
            Self::InvalidSamplesPerPixel => "raw compressed tile sample count must be positive",
            Self::EmptyData => "raw compressed tile payload data must not be empty",
        };
        f.write_str(message)
    }
}

impl Error for RawCompressedTileBuildError {}

impl From<RawCompressedTileBuildError> for WsiError {
    fn from(err: RawCompressedTileBuildError) -> Self {
        WsiError::Unsupported {
            reason: format!("invalid raw compressed tile metadata: {err}"),
        }
    }
}
