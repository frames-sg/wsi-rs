use super::*;

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

fn into_owned_vec<T: Clone>(samples: Arc<Vec<T>>) -> Vec<T> {
    Arc::try_unwrap(samples).unwrap_or_else(|shared| shared.as_ref().clone())
}

impl CpuTile {
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
                Ok(image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .expect("validated RGBA sample count matches image dimensions"))
            }
            ColorSpace::Rgb if self.channels == 3 => {
                let pixel_count = self.width as usize * self.height as usize;
                let mut rgba = Vec::with_capacity(pixel_count * 4);
                for idx in 0..pixel_count {
                    rgba.extend_from_slice(&self.u8_triplet_at(bytes, idx)?);
                    rgba.push(255);
                }
                Ok(image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .expect("validated RGB sample count expands to exact RGBA dimensions"))
            }
            ColorSpace::Grayscale if self.channels == 1 => {
                let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
                for &val in bytes {
                    rgba.extend_from_slice(&[val, val, val, 255]);
                }
                Ok(image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .expect("validated grayscale samples expand to exact RGBA dimensions"))
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
                Ok(image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .expect("validated YCbCr samples expand to exact RGBA dimensions"))
            }
            ColorSpace::Palette(lut) if self.channels == 1 => {
                let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
                for &idx in bytes {
                    let rgb = lut.get(idx as usize).unwrap_or(&[0, 0, 0]);
                    rgba.extend_from_slice(rgb);
                    rgba.push(255);
                }
                Ok(image::RgbaImage::from_raw(self.width, self.height, rgba)
                    .expect("validated palette samples expand to exact RGBA dimensions"))
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
            } => Ok(
                image::RgbaImage::from_raw(width, height, into_owned_vec(bytes))
                    .expect("validated RGBA storage matches image dimensions"),
            ),
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

        Ok(image::RgbaImage::from_raw(self.width, self.height, rgba)
            .expect("windowed samples produce exactly one RGBA pixel per input pixel"))
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
                Ok(
                    image::RgbImage::from_raw(self.width, self.height, bytes.to_vec())
                        .expect("validated RGB samples match image dimensions"),
                )
            }
            (ColorSpace::Grayscale, 1, _) => {
                let pixel_count = self.width as usize * self.height as usize;
                let mut rgb_data = Vec::with_capacity(pixel_count * 3);
                for &val in bytes {
                    rgb_data.extend_from_slice(&[val, val, val]);
                }
                Ok(image::RgbImage::from_raw(self.width, self.height, rgb_data)
                    .expect("validated grayscale samples expand to exact RGB dimensions"))
            }
            _ => {
                // Fallback: go through RGBA and strip alpha
                let rgba = self.to_rgba()?;
                let (w, h) = rgba.dimensions();
                let mut rgb_data = Vec::with_capacity((w * h * 3) as usize);
                for pixel in rgba.pixels() {
                    rgb_data.extend_from_slice(&pixel.0[..3]);
                }
                Ok(image::RgbImage::from_raw(w, h, rgb_data)
                    .expect("RGBA fallback strips exactly one alpha sample per pixel"))
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
            } => Ok(
                image::RgbImage::from_raw(width, height, into_owned_vec(bytes))
                    .expect("validated RGB storage matches image dimensions"),
            ),
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

        Ok(image::RgbImage::from_raw(self.width, self.height, rgb)
            .expect("windowed samples produce exactly one RGB pixel per input pixel"))
    }
}
