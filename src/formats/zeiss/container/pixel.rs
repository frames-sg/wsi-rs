use crate::core::types::Compression;
use crate::error::WsiError;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) enum PixelType {
    Gray8,
    Gray16,
    Gray32Float,
    Bgr24,
    Bgr48,
    Bgr96Float,
    Bgra32,
    Gray64ComplexFloat,
    Bgr192ComplexFloat,
    Gray32,
    Gray64Float,
}

impl PixelType {
    pub(in crate::formats::zeiss) fn from_raw(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Gray8),
            1 => Some(Self::Gray16),
            2 => Some(Self::Gray32Float),
            3 => Some(Self::Bgr24),
            4 => Some(Self::Bgr48),
            8 => Some(Self::Bgr96Float),
            9 => Some(Self::Bgra32),
            10 => Some(Self::Gray64ComplexFloat),
            11 => Some(Self::Bgr192ComplexFloat),
            12 => Some(Self::Gray32),
            13 => Some(Self::Gray64Float),
            _ => None,
        }
    }

    pub(in crate::formats::zeiss) fn from_name(name: &str) -> Option<Self> {
        match name.trim() {
            "Gray8" => Some(Self::Gray8),
            "Gray16" => Some(Self::Gray16),
            "Gray32Float" => Some(Self::Gray32Float),
            "Bgr24" => Some(Self::Bgr24),
            "Bgr48" => Some(Self::Bgr48),
            "Bgr96Float" => Some(Self::Bgr96Float),
            "Bgra32" => Some(Self::Bgra32),
            "Gray64ComplexFloat" => Some(Self::Gray64ComplexFloat),
            "Bgr192ComplexFloat" => Some(Self::Bgr192ComplexFloat),
            "Gray32" => Some(Self::Gray32),
            "Gray64Float" => Some(Self::Gray64Float),
            _ => None,
        }
    }

    pub(in crate::formats::zeiss) fn as_str(self) -> &'static str {
        match self {
            Self::Gray8 => "Gray8",
            Self::Gray16 => "Gray16",
            Self::Gray32Float => "Gray32Float",
            Self::Bgr24 => "Bgr24",
            Self::Bgr48 => "Bgr48",
            Self::Bgr96Float => "Bgr96Float",
            Self::Bgra32 => "Bgra32",
            Self::Gray64ComplexFloat => "Gray64ComplexFloat",
            Self::Bgr192ComplexFloat => "Bgr192ComplexFloat",
            Self::Gray32 => "Gray32",
            Self::Gray64Float => "Gray64Float",
        }
    }

    pub(in crate::formats::zeiss) fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::Gray16 => 2,
            Self::Gray32Float | Self::Gray32 | Self::Bgra32 => 4,
            Self::Bgr24 => 3,
            Self::Bgr48 => 6,
            Self::Bgr96Float => 12,
            Self::Gray64ComplexFloat => 16,
            Self::Bgr192ComplexFloat => 24,
            Self::Gray64Float => 8,
        }
    }
}

impl std::fmt::Display for PixelType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) enum CompressionMode {
    UnCompressed,
    Jpg,
    JpgXr,
    Zstd0,
    Zstd1,
}

impl CompressionMode {
    /// Whether WSI and embedded-image reads can decode this subblock compression.
    pub(in crate::formats::zeiss) fn is_wsi_supported(self) -> bool {
        matches!(self, Self::UnCompressed | Self::Jpg | Self::JpgXr)
    }

    pub(in crate::formats::zeiss) fn compression(self) -> Compression {
        match self {
            Self::UnCompressed => Compression::None,
            Self::Jpg => Compression::Jpeg,
            Self::JpgXr => Compression::JpegXr,
            Self::Zstd0 | Self::Zstd1 => Compression::Zstd,
        }
    }

    pub(in crate::formats::zeiss) fn from_raw(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::UnCompressed),
            1 => Some(Self::Jpg),
            4 => Some(Self::JpgXr),
            5 => Some(Self::Zstd0),
            6 => Some(Self::Zstd1),
            _ => None,
        }
    }

    pub(in crate::formats::zeiss) fn as_str(self) -> &'static str {
        match self {
            Self::UnCompressed => "UnCompressed",
            Self::Jpg => "Jpg",
            Self::JpgXr => "JpgXr",
            Self::Zstd0 => "Zstd0",
            Self::Zstd1 => "Zstd1",
        }
    }
}

impl std::fmt::Display for CompressionMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct Bitmap {
    pub(in crate::formats::zeiss) pixel_type: PixelType,
    pub(in crate::formats::zeiss) width: u32,
    pub(in crate::formats::zeiss) height: u32,
    pub(in crate::formats::zeiss) stride: usize,
    pub(in crate::formats::zeiss) data: Vec<u8>,
}

impl Bitmap {
    pub(in crate::formats::zeiss) fn new(
        pixel_type: PixelType,
        width: u32,
        height: u32,
        data: Vec<u8>,
    ) -> Result<Self, WsiError> {
        let stride = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(pixel_type.bytes_per_pixel()))
            .ok_or_else(|| invalid("CZI bitmap row size overflows"))?;
        let expected = stride
            .checked_mul(usize::try_from(height).unwrap_or(usize::MAX))
            .ok_or_else(|| invalid("CZI bitmap size overflows"))?;
        if data.len() != expected {
            return Err(invalid(format!(
                "CZI bitmap byte count mismatch: expected {expected}, got {}",
                data.len()
            )));
        }
        Ok(Self {
            pixel_type,
            width,
            height,
            stride,
            data,
        })
    }

    pub(in crate::formats::zeiss) fn zeros(
        pixel_type: PixelType,
        width: u32,
        height: u32,
    ) -> Result<Self, WsiError> {
        let stride = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(pixel_type.bytes_per_pixel()))
            .ok_or_else(|| invalid("CZI bitmap row size overflows"))?;
        let length = stride
            .checked_mul(usize::try_from(height).unwrap_or(usize::MAX))
            .ok_or_else(|| invalid("CZI bitmap size overflows"))?;
        Self::new(pixel_type, width, height, vec![0; length])
    }

    pub(in crate::formats::zeiss) fn to_u16_vec(&self) -> Result<Vec<u16>, WsiError> {
        let (samples, remainder) = self.data.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(invalid("CZI 16-bit bitmap has an odd byte count"));
        }
        Ok(samples.iter().copied().map(u16::from_le_bytes).collect())
    }
}

fn invalid(message: impl Into<String>) -> WsiError {
    WsiError::DisplayConversion(message.into())
}
