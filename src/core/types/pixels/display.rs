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

    fn windowed_rgb_bytes(
        &self,
        window: &DisplayWindow,
        include_alpha: bool,
    ) -> Result<Vec<u8>, WsiError> {
        self.validate_invariants()?;
        // DisplayWindow::new admits only finite bounds with max > min.
        let range = window.max - window.min;
        let pixel_count = (self.width as usize) * (self.height as usize);
        let output_channels = if include_alpha { 4 } else { 3 };
        let mut output = Vec::with_capacity(pixel_count * output_channels);
        let scale = |sample: f64| {
            (((sample - window.min) / range) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        let push_alpha = |output: &mut Vec<u8>| {
            if include_alpha {
                output.push(255);
            }
        };

        match &self.data {
            CpuTileData::U16(samples) => {
                if self.channels == 1 {
                    for &sample in samples.iter() {
                        let value = scale(f64::from(sample));
                        output.extend_from_slice(&[value, value, value]);
                        push_alpha(&mut output);
                    }
                } else if self.channels == 3 {
                    for idx in 0..pixel_count {
                        for sample in self.u16_triplet_at(samples, idx)? {
                            output.push(scale(f64::from(sample)));
                        }
                        push_alpha(&mut output);
                    }
                } else {
                    return Err(WsiError::DisplayConversion(format!(
                        "unsupported channel count {} for windowed conversion",
                        self.channels
                    )));
                }
            }
            CpuTileData::F32(samples) => {
                if self.channels == 1 {
                    for &sample in samples.iter() {
                        let value = scale(f64::from(sample));
                        output.extend_from_slice(&[value, value, value]);
                        push_alpha(&mut output);
                    }
                } else if self.channels == 3 && self.layout == CpuTileLayout::Interleaved {
                    for pixel in samples.as_chunks::<3>().0 {
                        for &sample in pixel {
                            output.push(scale(f64::from(sample)));
                        }
                        push_alpha(&mut output);
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
        Ok(output)
    }

    /// Convert Uint8 data to RgbaImage. Returns error for non-Uint8 data.
    pub fn to_rgba(&self) -> Result<image::RgbaImage, WsiError> {
        let bytes = self.data.as_u8().ok_or_else(|| {
            WsiError::DisplayConversion(
                "to_rgba() requires Uint8 data; use to_rgba_windowed() for Uint16/Float32".into(),
            )
        })?;
        self.validate_invariants()?;
        let rgba = self.u8_display_bytes::<true>(bytes)?;
        Ok(image::RgbaImage::from_raw(self.width, self.height, rgba)
            .expect("validated samples produce exactly one RGBA pixel per input pixel"))
    }

    /// Uint8 samples as interleaved RGB, followed by each pixel's alpha (255
    /// unless the source has alpha) when `INCLUDE_ALPHA` is set. The output
    /// shape is fixed at compilation, so pixels need no runtime alpha switch.
    fn u8_display_bytes<const INCLUDE_ALPHA: bool>(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<u8>, WsiError> {
        match (&self.color_space, self.channels) {
            (ColorSpace::Rgba, 4)
            | (ColorSpace::Rgb | ColorSpace::YCbCr, 3)
            | (ColorSpace::Grayscale | ColorSpace::Palette(_), 1) => {}
            (ColorSpace::Unknown, _) => {
                return Err(WsiError::DisplayConversion("unknown color space".into()));
            }
            (other, _) => {
                return Err(WsiError::DisplayConversion(format!(
                    "unsupported color space {:?} with {} channels for {}",
                    other,
                    self.channels,
                    if INCLUDE_ALPHA {
                        "to_rgba()"
                    } else {
                        "to_rgb()"
                    },
                )));
            }
        }
        let pixel_count = self.width as usize * self.height as usize;
        let mut output = Vec::with_capacity(pixel_count * if INCLUDE_ALPHA { 4 } else { 3 });
        let mut push = |rgb: [u8; 3], alpha: u8| {
            if INCLUDE_ALPHA {
                output.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
            } else {
                output.extend_from_slice(&rgb);
            }
        };
        match &self.color_space {
            ColorSpace::Rgba => match self.layout {
                CpuTileLayout::Interleaved if INCLUDE_ALPHA => {
                    output.extend_from_slice(bytes);
                }
                CpuTileLayout::Interleaved => {
                    for pixel in bytes.as_chunks::<4>().0 {
                        output.extend_from_slice(&pixel[..3]);
                    }
                }
                CpuTileLayout::Planar => {
                    let (red, rest) = bytes.split_at(pixel_count);
                    let (green, rest) = rest.split_at(pixel_count);
                    let (blue, alpha) = rest.split_at(pixel_count);
                    let pixels = red.iter().zip(green).zip(blue).zip(alpha);
                    if INCLUDE_ALPHA {
                        output.resize(pixel_count * 4, 0);
                        for (pixel, (((&r, &g), &b), &a)) in
                            output.as_chunks_mut::<4>().0.iter_mut().zip(pixels)
                        {
                            *pixel = [r, g, b, a];
                        }
                    } else {
                        for (((&r, &g), &b), &a) in pixels {
                            push([r, g, b], a);
                        }
                    }
                }
            },
            ColorSpace::Rgb => {
                for idx in 0..pixel_count {
                    push(self.u8_triplet_at(bytes, idx)?, 255);
                }
            }
            ColorSpace::Grayscale => {
                for &val in bytes {
                    push([val, val, val], 255);
                }
            }
            ColorSpace::YCbCr => {
                for idx in 0..pixel_count {
                    let [y, cb, cr] = self.u8_triplet_at(bytes, idx)?;
                    push(super::ycbcr::ycbcr_to_rgb_u8(y, cb, cr), 255);
                }
            }
            ColorSpace::Palette(lut) => {
                for &idx in bytes {
                    push(*lut.get(idx as usize).unwrap_or(&[0, 0, 0]), 255);
                }
            }
            ColorSpace::Unknown | ColorSpace::Cmyk => {
                unreachable!("unsupported color space rejected before allocation")
            }
        }
        Ok(output)
    }

    /// Convert this buffer into an owned RgbaImage, reusing the underlying
    /// byte vector directly when the buffer is already RGBA8 interleaved.
    pub fn into_rgba(self) -> Result<image::RgbaImage, WsiError> {
        if let CpuTileData::U8(_) = &self.data {
            self.validate_invariants()?;
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
        let rgba = self.windowed_rgb_bytes(window, true)?;
        Ok(image::RgbaImage::from_raw(self.width, self.height, rgba)
            .expect("windowed samples produce exactly one RGBA pixel per input pixel"))
    }

    /// Convert Uint8 data to RgbImage without an intermediate RGBA image.
    pub fn to_rgb(&self) -> Result<image::RgbImage, WsiError> {
        let bytes = self.data.as_u8().ok_or_else(|| {
            WsiError::DisplayConversion(
                "to_rgb() requires Uint8 data; use to_rgb_windowed() for Uint16/Float32".into(),
            )
        })?;
        self.validate_invariants()?;

        match (&self.color_space, self.channels, self.layout) {
            (ColorSpace::Rgb, 3, CpuTileLayout::Interleaved) => {
                Ok(
                    image::RgbImage::from_raw(self.width, self.height, bytes.to_vec())
                        .expect("validated RGB samples match image dimensions"),
                )
            }
            _ => {
                let rgb = self.u8_display_bytes::<false>(bytes)?;
                Ok(image::RgbImage::from_raw(self.width, self.height, rgb)
                    .expect("validated samples produce exactly one RGB pixel per input pixel"))
            }
        }
    }

    /// Convert this buffer into an owned RgbImage, reusing the underlying
    /// byte vector directly when the buffer is already RGB8 interleaved.
    pub fn into_rgb(self) -> Result<image::RgbImage, WsiError> {
        if let CpuTileData::U8(_) = &self.data {
            self.validate_invariants()?;
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
        let rgb = self.windowed_rgb_bytes(window, false)?;
        Ok(image::RgbImage::from_raw(self.width, self.height, rgb)
            .expect("windowed samples produce exactly one RGB pixel per input pixel"))
    }
}
