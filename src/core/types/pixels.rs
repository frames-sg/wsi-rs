use super::*;

mod display;

pub use display::DisplayWindow;

// ── Sample types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum SampleType {
    Uint8,
    Uint16,
    Float32,
}

impl SampleType {
    pub fn byte_size(&self) -> usize {
        match self {
            SampleType::Uint8 => 1,
            SampleType::Uint16 => 2,
            SampleType::Float32 => 4,
        }
    }
}

/// Concrete pixel format for decoded CPU and device-resident surfaces.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum PixelFormat {
    Rgb8,
    Rgba8,
    Gray8,
    Rgb16,
    Rgba16,
    Gray16,
}

impl PixelFormat {
    pub const fn color_space(self) -> ColorSpace {
        match self {
            Self::Rgb8 | Self::Rgb16 => ColorSpace::Rgb,
            Self::Rgba8 | Self::Rgba16 => ColorSpace::Rgba,
            Self::Gray8 | Self::Gray16 => ColorSpace::Grayscale,
        }
    }

    pub const fn sample_type(self) -> SampleType {
        match self {
            Self::Rgb8 | Self::Rgba8 | Self::Gray8 => SampleType::Uint8,
            Self::Rgb16 | Self::Rgba16 | Self::Gray16 => SampleType::Uint16,
        }
    }

    pub const fn channels(self) -> usize {
        match self {
            Self::Rgb8 | Self::Rgb16 => 3,
            Self::Rgba8 | Self::Rgba16 => 4,
            Self::Gray8 | Self::Gray16 => 1,
        }
    }

    pub const fn bytes_per_sample(self) -> usize {
        match self.sample_type() {
            SampleType::Uint8 => 1,
            SampleType::Uint16 => 2,
            SampleType::Float32 => 4,
        }
    }

    pub const fn bytes_per_pixel(self) -> usize {
        self.channels() * self.bytes_per_sample()
    }
}

#[cfg(any(feature = "metal", feature = "cuda"))]
impl TryFrom<j2k_core::PixelFormat> for PixelFormat {
    type Error = WsiError;

    fn try_from(format: j2k_core::PixelFormat) -> Result<Self, Self::Error> {
        match format {
            j2k_core::PixelFormat::Rgb8 => Ok(Self::Rgb8),
            j2k_core::PixelFormat::Rgba8 => Ok(Self::Rgba8),
            j2k_core::PixelFormat::Gray8 => Ok(Self::Gray8),
            j2k_core::PixelFormat::Rgb16 => Ok(Self::Rgb16),
            j2k_core::PixelFormat::Rgba16 => Ok(Self::Rgba16),
            j2k_core::PixelFormat::Gray16 => Ok(Self::Gray16),
            _ => Err(WsiError::Unsupported {
                reason: format!("pixel format {format:?} is unsupported by wsi-rs"),
            }),
        }
    }
}

#[cfg(any(feature = "metal", feature = "cuda"))]
impl From<PixelFormat> for j2k_core::PixelFormat {
    fn from(format: PixelFormat) -> Self {
        match format {
            PixelFormat::Rgb8 => Self::Rgb8,
            PixelFormat::Rgba8 => Self::Rgba8,
            PixelFormat::Gray8 => Self::Gray8,
            PixelFormat::Rgb16 => Self::Rgb16,
            PixelFormat::Rgba16 => Self::Rgba16,
            PixelFormat::Gray16 => Self::Gray16,
        }
    }
}

#[cfg(all(test, any(feature = "metal", feature = "cuda")))]
#[path = "tests/j2k_format_tests.rs"]
mod j2k_format_tests;

/// Typed, aligned sample storage.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CpuTileData {
    U8(Arc<Vec<u8>>),
    U16(Arc<Vec<u16>>),
    F32(Arc<Vec<f32>>),
}

impl CpuTileData {
    pub fn u8(samples: Vec<u8>) -> Self {
        Self::U8(Arc::new(samples))
    }

    pub fn u16(samples: Vec<u16>) -> Self {
        Self::U16(Arc::new(samples))
    }

    pub fn f32(samples: Vec<f32>) -> Self {
        Self::F32(Arc::new(samples))
    }

    pub fn sample_type(&self) -> SampleType {
        match self {
            CpuTileData::U8(_) => SampleType::Uint8,
            CpuTileData::U16(_) => SampleType::Uint16,
            CpuTileData::F32(_) => SampleType::Float32,
        }
    }

    pub fn byte_size(&self) -> usize {
        match self {
            CpuTileData::U8(v) => v.len(),
            CpuTileData::U16(v) => v.len() * 2,
            CpuTileData::F32(v) => v.len() * 4,
        }
    }

    pub fn as_u8(&self) -> Option<&[u8]> {
        match self {
            CpuTileData::U8(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    pub fn as_u16(&self) -> Option<&[u16]> {
        match self {
            CpuTileData::U16(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<&[f32]> {
        match self {
            CpuTileData::F32(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    pub fn make_mut_u8(&mut self) -> Option<&mut Vec<u8>> {
        match self {
            CpuTileData::U8(v) => Some(Arc::make_mut(v)),
            _ => None,
        }
    }

    pub fn make_mut_u16(&mut self) -> Option<&mut Vec<u16>> {
        match self {
            CpuTileData::U16(v) => Some(Arc::make_mut(v)),
            _ => None,
        }
    }

    pub fn make_mut_f32(&mut self) -> Option<&mut Vec<f32>> {
        match self {
            CpuTileData::F32(v) => Some(Arc::make_mut(v)),
            _ => None,
        }
    }
}

/// Declared color model.
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum ColorSpace {
    Rgb,
    Rgba,
    Grayscale,
    YCbCr,
    Cmyk,
    /// Indexed color with LUT entries as [R, G, B] triples.
    Palette(Arc<Vec<[u8; 3]>>),
    Unknown,
}

/// Whether channel samples are interleaved or planar.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum CpuTileLayout {
    Interleaved,
    Planar,
}

/// Generic decoded pixel buffer.
///
/// **Invariant:** `data` length must equal `width * height * channels` in samples.
/// Use [`CpuTile::new()`] to construct with validation, and use the read
/// accessors for metadata and pixel storage.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CpuTile {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) channels: u16,
    pub(crate) color_space: ColorSpace,
    pub(crate) layout: CpuTileLayout,
    pub(crate) data: CpuTileData,
}

impl CpuTile {
    /// Construct a CpuTile, validating that the data length matches
    /// `width * height * channels`. Uses checked arithmetic to prevent
    /// overflow on large dimensions.
    pub fn new(
        width: u32,
        height: u32,
        channels: u16,
        color_space: ColorSpace,
        layout: CpuTileLayout,
        data: CpuTileData,
    ) -> Result<Self, WsiError> {
        let tile = Self {
            width,
            height,
            channels,
            color_space,
            layout,
            data,
        };
        tile.validate_invariants()?;
        Ok(tile)
    }

    pub(crate) fn validate_invariants(&self) -> Result<(), WsiError> {
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|wh| wh.checked_mul(self.channels as usize))
            .ok_or_else(|| {
                WsiError::DisplayConversion(format!(
                    "CpuTile dimensions overflow: {}x{}x{}",
                    self.width, self.height, self.channels,
                ))
            })?;
        let actual = match &self.data {
            CpuTileData::U8(v) => v.len(),
            CpuTileData::U16(v) => v.len(),
            CpuTileData::F32(v) => v.len(),
        };
        if actual != expected {
            return Err(WsiError::DisplayConversion(format!(
                "CpuTile invariant violated: {}x{}x{} = {} samples, but data has {}",
                self.width, self.height, self.channels, expected, actual,
            )));
        }
        Ok(())
    }

    /// Construct an interleaved U8 CPU tile.
    pub fn from_u8_interleaved(
        width: u32,
        height: u32,
        channels: u16,
        color_space: ColorSpace,
        pixels: Vec<u8>,
    ) -> Result<Self, WsiError> {
        Self::new(
            width,
            height,
            channels,
            color_space,
            CpuTileLayout::Interleaved,
            CpuTileData::u8(pixels),
        )
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn color_space(&self) -> &ColorSpace {
        &self.color_space
    }

    pub fn layout(&self) -> CpuTileLayout {
        self.layout
    }

    pub fn data(&self) -> &CpuTileData {
        &self.data
    }

    pub fn stride_bytes(&self) -> usize {
        self.width as usize * self.channels as usize * self.data.sample_type().byte_size()
    }

    pub fn as_u8(&self) -> Option<&[u8]> {
        self.data.as_u8()
    }

    /// Clones the shared U8 sample storage without copying its pixels.
    ///
    /// Returns `None` for U16 and F32 tiles. The returned [`Arc`] points to the
    /// same `Vec<u8>` allocation held by this tile, so mutations use the
    /// copy-on-write behavior provided by [`CpuTileData::make_mut_u8`].
    /// Callers migrating from `Arc<[u8]>` can call `as_slice()` on the returned
    /// `Arc<Vec<u8>>` when they only need slice access.
    pub fn pixels_arc(&self) -> Option<Arc<Vec<u8>>> {
        match &self.data {
            CpuTileData::U8(pixels) => Some(Arc::clone(pixels)),
            CpuTileData::U16(_) | CpuTileData::F32(_) => None,
        }
    }
}
