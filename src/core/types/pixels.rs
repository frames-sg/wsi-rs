use super::*;

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

    #[cfg(any(feature = "metal", feature = "cuda"))]
    pub(crate) fn try_from_signinum(format: signinum_core::PixelFormat) -> Result<Self, WsiError> {
        match format {
            signinum_core::PixelFormat::Rgb8 => Ok(Self::Rgb8),
            signinum_core::PixelFormat::Rgba8 => Ok(Self::Rgba8),
            signinum_core::PixelFormat::Gray8 => Ok(Self::Gray8),
            signinum_core::PixelFormat::Rgb16 => Ok(Self::Rgb16),
            signinum_core::PixelFormat::Rgba16 => Ok(Self::Rgba16),
            signinum_core::PixelFormat::Gray16 => Ok(Self::Gray16),
            _ => Err(WsiError::Unsupported {
                reason: format!("device decode returned unsupported pixel format {format:?}"),
            }),
        }
    }

    #[cfg(all(test, feature = "metal"))]
    pub(crate) const fn to_signinum(self) -> signinum_core::PixelFormat {
        match self {
            Self::Rgb8 => signinum_core::PixelFormat::Rgb8,
            Self::Rgba8 => signinum_core::PixelFormat::Rgba8,
            Self::Gray8 => signinum_core::PixelFormat::Gray8,
            Self::Rgb16 => signinum_core::PixelFormat::Rgb16,
            Self::Rgba16 => signinum_core::PixelFormat::Rgba16,
            Self::Gray16 => signinum_core::PixelFormat::Gray16,
        }
    }
}

/// Typed, aligned sample storage.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CpuTileData {
    U8(Arc<Vec<u8>>),
    U16(Arc<Vec<u16>>),
    F32(Arc<Vec<f32>>),
}

fn into_owned_vec<T: Clone>(samples: Arc<Vec<T>>) -> Vec<T> {
    Arc::try_unwrap(samples).unwrap_or_else(|shared| shared.as_ref().clone())
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

/// Windowing parameters for high-dynamic-range display conversion.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DisplayWindow {
    min: f64,
    max: f64,
}

impl DisplayWindow {
    /// Creates a display window with finite bounds and a positive range.
    pub fn new(min: f64, max: f64) -> Result<Self, WsiError> {
        if !min.is_finite() || !max.is_finite() {
            return Err(WsiError::DisplayConversion(
                "window bounds must be finite".into(),
            ));
        }
        if max <= min {
            return Err(WsiError::DisplayConversion(
                "window range must be positive".into(),
            ));
        }
        Ok(Self { min, max })
    }

    pub fn min(&self) -> f64 {
        self.min
    }

    pub fn max(&self) -> f64 {
        self.max
    }
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
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|wh| wh.checked_mul(channels as usize))
            .ok_or_else(|| {
                WsiError::DisplayConversion(format!(
                    "CpuTile dimensions overflow: {}x{}x{}",
                    width, height, channels,
                ))
            })?;
        let actual = match &data {
            CpuTileData::U8(v) => v.len(),
            CpuTileData::U16(v) => v.len(),
            CpuTileData::F32(v) => v.len(),
        };
        if actual != expected {
            return Err(WsiError::DisplayConversion(format!(
                "CpuTile invariant violated: {}x{}x{} = {} samples, but data has {}",
                width, height, channels, expected, actual,
            )));
        }
        Ok(Self {
            width,
            height,
            channels,
            color_space,
            layout,
            data,
        })
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

    /// Test/support constructor for code that already owns byte-slice storage.
    #[cfg(test)]
    pub(crate) fn new_for_test(
        pixels: Arc<[u8]>,
        width: u32,
        height: u32,
        stride_bytes: usize,
        format: PixelFormat,
    ) -> Self {
        let channels = format.channels() as u16;
        let row_min = width as usize * format.bytes_per_pixel();
        assert!(
            stride_bytes >= row_min,
            "stride_bytes={stride_bytes} < row_min={row_min}"
        );
        assert_eq!(
            stride_bytes, row_min,
            "CpuTile::new_for_test currently stores packed interleaved data; use sv_tile::SlideCpuTile for padded test storage until statumen CpuTile is reshaped"
        );
        let expected = stride_bytes
            .checked_mul(height as usize)
            .expect("test tile dimensions overflow");
        assert!(
            pixels.len() >= expected,
            "pixels len {} < expected {}",
            pixels.len(),
            expected
        );
        Self::new(
            width,
            height,
            channels,
            format.color_space(),
            CpuTileLayout::Interleaved,
            CpuTileData::u8(pixels.as_ref().to_vec()),
        )
        .expect("validated test tile")
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

    pub fn pixels_arc(&self) -> Option<Arc<[u8]>> {
        self.data.as_u8().map(Arc::<[u8]>::from)
    }

    fn expected_samples(&self) -> usize {
        self.width as usize * self.height as usize * self.channels as usize
    }

    fn validate_len<T>(&self, samples: &[T]) -> Result<(), WsiError> {
        if samples.len() == self.expected_samples() {
            Ok(())
        } else {
            Err(WsiError::DisplayConversion(format!(
                "buffer size mismatch: expected {} samples, got {}",
                self.expected_samples(),
                samples.len()
            )))
        }
    }

    fn u8_triplet_at(&self, bytes: &[u8], idx: usize) -> Result<[u8; 3], WsiError> {
        match self.layout {
            CpuTileLayout::Interleaved => {
                let base = idx * 3;
                Ok([bytes[base], bytes[base + 1], bytes[base + 2]])
            }
            CpuTileLayout::Planar => {
                let plane = self.width as usize * self.height as usize;
                Ok([bytes[idx], bytes[plane + idx], bytes[2 * plane + idx]])
            }
        }
    }

    fn u8_quad_at(&self, bytes: &[u8], idx: usize) -> Result<[u8; 4], WsiError> {
        match self.layout {
            CpuTileLayout::Interleaved => {
                let base = idx * 4;
                Ok([
                    bytes[base],
                    bytes[base + 1],
                    bytes[base + 2],
                    bytes[base + 3],
                ])
            }
            CpuTileLayout::Planar => {
                let plane = self.width as usize * self.height as usize;
                Ok([
                    bytes[idx],
                    bytes[plane + idx],
                    bytes[2 * plane + idx],
                    bytes[3 * plane + idx],
                ])
            }
        }
    }

    fn u16_triplet_at(&self, samples: &[u16], idx: usize) -> Result<[u16; 3], WsiError> {
        match self.layout {
            CpuTileLayout::Interleaved => {
                let base = idx * 3;
                Ok([samples[base], samples[base + 1], samples[base + 2]])
            }
            CpuTileLayout::Planar => {
                let plane = self.width as usize * self.height as usize;
                Ok([samples[idx], samples[plane + idx], samples[2 * plane + idx]])
            }
        }
    }

    /// Convert Uint8 data to RgbaImage. Returns error for non-Uint8 data.
    pub fn to_rgba(&self) -> Result<image::RgbaImage, WsiError> {
        let bytes = self.data.as_u8().ok_or_else(|| {
            WsiError::DisplayConversion(
                "to_rgba() requires Uint8 data; use to_rgba_windowed() for Uint16/Float32".into(),
            )
        })?;
        self.validate_len(bytes)?;
        match &self.color_space {
            ColorSpace::Rgba if self.channels == 4 => {
                let pixel_count = self.width as usize * self.height as usize;
                let mut rgba = Vec::with_capacity(pixel_count * 4);
                for idx in 0..pixel_count {
                    rgba.extend_from_slice(&self.u8_quad_at(bytes, idx)?);
                }
                image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
            }
            ColorSpace::Rgb if self.channels == 3 => {
                let pixel_count = self.width as usize * self.height as usize;
                let mut rgba = Vec::with_capacity(pixel_count * 4);
                for idx in 0..pixel_count {
                    rgba.extend_from_slice(&self.u8_triplet_at(bytes, idx)?);
                    rgba.push(255);
                }
                image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
            }
            ColorSpace::Grayscale if self.channels == 1 => {
                let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
                for &val in bytes {
                    rgba.extend_from_slice(&[val, val, val, 255]);
                }
                image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
            }
            ColorSpace::YCbCr if self.channels == 3 => {
                let pixel_count = self.width as usize * self.height as usize;
                let mut rgba = Vec::with_capacity(pixel_count * 4);
                for idx in 0..pixel_count {
                    let [y_raw, cb_raw, cr_raw] = self.u8_triplet_at(bytes, idx)?;
                    let y = y_raw as f64;
                    let cb = cb_raw as f64 - 128.0;
                    let cr = cr_raw as f64 - 128.0;
                    let r = (y + 1.402 * cr).round().clamp(0.0, 255.0) as u8;
                    let g = (y - 0.344136 * cb - 0.714136 * cr)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    let b = (y + 1.772 * cb).round().clamp(0.0, 255.0) as u8;
                    rgba.extend_from_slice(&[r, g, b, 255]);
                }
                image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
            }
            ColorSpace::Palette(lut) if self.channels == 1 => {
                let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
                for &idx in bytes {
                    let rgb = lut.get(idx as usize).unwrap_or(&[0, 0, 0]);
                    rgba.extend_from_slice(rgb);
                    rgba.push(255);
                }
                image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
            }
            ColorSpace::Unknown => Err(WsiError::DisplayConversion("unknown color space".into())),
            other => Err(WsiError::DisplayConversion(format!(
                "unsupported color space {:?} with {} channels for to_rgba()",
                other, self.channels
            ))),
        }
    }

    /// Convert this buffer into an owned RgbaImage, reusing the underlying
    /// byte vector directly when the buffer is already RGBA8 interleaved.
    pub fn into_rgba(self) -> Result<image::RgbaImage, WsiError> {
        if let CpuTileData::U8(bytes) = &self.data {
            self.validate_len(bytes)?;
        }
        match self {
            CpuTile {
                width,
                height,
                channels: 4,
                color_space: ColorSpace::Rgba,
                layout: CpuTileLayout::Interleaved,
                data: CpuTileData::U8(bytes),
            } => image::RgbaImage::from_raw(width, height, into_owned_vec(bytes))
                .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into())),
            buffer => buffer.to_rgba(),
        }
    }

    /// Convert any sample type to RgbaImage with explicit windowing.
    pub fn to_rgba_windowed(&self, window: &DisplayWindow) -> Result<image::RgbaImage, WsiError> {
        if let CpuTileData::U8(_) = &self.data {
            return self.to_rgba();
        }
        let range = window.max - window.min;
        if range <= 0.0 {
            return Err(WsiError::DisplayConversion(
                "window range must be positive".into(),
            ));
        }
        let pixel_count = (self.width as usize) * (self.height as usize);
        let mut rgba = Vec::with_capacity(pixel_count * 4);

        match &self.data {
            CpuTileData::U16(samples) => {
                self.validate_len(samples)?;
                if self.channels == 1 {
                    for &s in samples.iter() {
                        let v = (((s as f64 - window.min) / range) * 255.0)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        rgba.extend_from_slice(&[v, v, v, 255]);
                    }
                } else if self.channels == 3 {
                    for idx in 0..pixel_count {
                        for s in self.u16_triplet_at(samples, idx)? {
                            let v = (((s as f64 - window.min) / range) * 255.0)
                                .round()
                                .clamp(0.0, 255.0) as u8;
                            rgba.push(v);
                        }
                        rgba.push(255);
                    }
                } else {
                    return Err(WsiError::DisplayConversion(format!(
                        "unsupported channel count {} for windowed conversion",
                        self.channels
                    )));
                }
            }
            CpuTileData::F32(samples) => {
                self.validate_len(samples)?;
                if self.channels == 1 {
                    for &s in samples.iter() {
                        let v = (((s as f64 - window.min) / range) * 255.0)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        rgba.extend_from_slice(&[v, v, v, 255]);
                    }
                } else if self.channels == 3 && self.layout == CpuTileLayout::Interleaved {
                    for pixel in samples.chunks_exact(3) {
                        for &s in pixel {
                            let v = (((s as f64 - window.min) / range) * 255.0)
                                .round()
                                .clamp(0.0, 255.0) as u8;
                            rgba.push(v);
                        }
                        rgba.push(255);
                    }
                } else {
                    return Err(WsiError::DisplayConversion(format!(
                        "unsupported channel count {} for F32 windowed conversion",
                        self.channels
                    )));
                }
            }
            CpuTileData::U8(_) => {
                return Err(WsiError::DisplayConversion(
                    "U8 data should not reach windowed conversion path".into(),
                ));
            }
        }

        image::RgbaImage::from_raw(self.width, self.height, rgba)
            .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
    }

    /// Convert Uint8 data to RgbImage. Direct path for RGB8 and Grayscale;
    /// other color spaces fall through RGBA conversion.
    pub fn to_rgb(&self) -> Result<image::RgbImage, WsiError> {
        let bytes = self.data.as_u8().ok_or_else(|| {
            WsiError::DisplayConversion(
                "to_rgb() requires Uint8 data; use to_rgb_windowed() for Uint16/Float32".into(),
            )
        })?;
        self.validate_len(bytes)?;

        match (&self.color_space, self.channels, self.layout) {
            (ColorSpace::Rgb, 3, CpuTileLayout::Interleaved) => {
                image::RgbImage::from_raw(self.width, self.height, bytes.to_vec())
                    .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
            }
            (ColorSpace::Grayscale, 1, _) => {
                let pixel_count = self.width as usize * self.height as usize;
                let mut rgb_data = Vec::with_capacity(pixel_count * 3);
                for &val in bytes {
                    rgb_data.extend_from_slice(&[val, val, val]);
                }
                image::RgbImage::from_raw(self.width, self.height, rgb_data)
                    .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
            }
            _ => {
                // Fallback: go through RGBA and strip alpha
                let rgba = self.to_rgba()?;
                let (w, h) = rgba.dimensions();
                let mut rgb_data = Vec::with_capacity((w * h * 3) as usize);
                for pixel in rgba.pixels() {
                    rgb_data.extend_from_slice(&pixel.0[..3]);
                }
                image::RgbImage::from_raw(w, h, rgb_data)
                    .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
            }
        }
    }

    /// Convert this buffer into an owned RgbImage, reusing the underlying
    /// byte vector directly when the buffer is already RGB8 interleaved.
    pub fn into_rgb(self) -> Result<image::RgbImage, WsiError> {
        if let CpuTileData::U8(bytes) = &self.data {
            self.validate_len(bytes)?;
        }
        match self {
            CpuTile {
                width,
                height,
                channels: 3,
                color_space: ColorSpace::Rgb,
                layout: CpuTileLayout::Interleaved,
                data: CpuTileData::U8(bytes),
            } => image::RgbImage::from_raw(width, height, into_owned_vec(bytes))
                .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into())),
            buffer => buffer.to_rgb(),
        }
    }

    /// Convert any sample type to RgbImage with explicit windowing.
    /// Direct path avoids intermediate RGBA allocation.
    pub fn to_rgb_windowed(&self, window: &DisplayWindow) -> Result<image::RgbImage, WsiError> {
        if let CpuTileData::U8(_) = &self.data {
            return self.to_rgb();
        }
        let range = window.max - window.min;
        if range <= 0.0 {
            return Err(WsiError::DisplayConversion(
                "window range must be positive".into(),
            ));
        }
        let pixel_count = (self.width as usize) * (self.height as usize);
        let mut rgb = Vec::with_capacity(pixel_count * 3);

        match &self.data {
            CpuTileData::U16(samples) => {
                self.validate_len(samples)?;
                if self.channels == 1 {
                    for &s in samples.iter() {
                        let v = (((s as f64 - window.min) / range) * 255.0)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        rgb.extend_from_slice(&[v, v, v]);
                    }
                } else if self.channels == 3 {
                    for idx in 0..pixel_count {
                        for s in self.u16_triplet_at(samples, idx)? {
                            let v = (((s as f64 - window.min) / range) * 255.0)
                                .round()
                                .clamp(0.0, 255.0) as u8;
                            rgb.push(v);
                        }
                    }
                } else {
                    return Err(WsiError::DisplayConversion(format!(
                        "unsupported channel count {} for windowed conversion",
                        self.channels
                    )));
                }
            }
            CpuTileData::F32(samples) => {
                self.validate_len(samples)?;
                if self.channels == 1 {
                    for &s in samples.iter() {
                        let v = (((s as f64 - window.min) / range) * 255.0)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        rgb.extend_from_slice(&[v, v, v]);
                    }
                } else if self.channels == 3 && self.layout == CpuTileLayout::Interleaved {
                    for pixel in samples.chunks_exact(3) {
                        for &s in pixel {
                            let v = (((s as f64 - window.min) / range) * 255.0)
                                .round()
                                .clamp(0.0, 255.0) as u8;
                            rgb.push(v);
                        }
                    }
                } else {
                    return Err(WsiError::DisplayConversion(format!(
                        "unsupported channel count {} for F32 windowed conversion",
                        self.channels
                    )));
                }
            }
            CpuTileData::U8(_) => {
                return Err(WsiError::DisplayConversion(
                    "U8 data should not reach windowed conversion path".into(),
                ));
            }
        }

        image::RgbImage::from_raw(self.width, self.height, rgb)
            .ok_or_else(|| WsiError::DisplayConversion("buffer size mismatch".into()))
    }
}
