use crate::{error::WsiError, PixelFormat};

use super::{interop, YcbcrToRgb8Converter};

/// Metal-backed device tile returned from `TilePixels::Device`.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct MetalDeviceTile {
    /// Compatibility mirror of the resident image width.
    pub width: u32,
    /// Compatibility mirror of the resident image height.
    pub height: u32,
    /// Compatibility mirror of the resident image row pitch.
    pub pitch_bytes: usize,
    /// Compatibility mirror of the resident image pixel format.
    pub format: PixelFormat,
    pub storage: MetalDeviceStorage,
}

/// Concrete Metal storage backing a [`MetalDeviceTile`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum MetalDeviceStorage {
    #[deprecated(note = "raw Buffer storage is untrusted; adopt it into Resident storage")]
    Buffer {
        buffer: metal::Buffer,
        byte_offset: usize,
    },
    Resident {
        image: j2k_metal_support::ResidentMetalImage,
    },
}

impl MetalDeviceTile {
    /// Build a Metal device tile from an opaque immutable resident image.
    pub fn from_resident(image: j2k_metal_support::ResidentMetalImage) -> Result<Self, WsiError> {
        Ok(Self {
            width: image.dimensions().0,
            height: image.dimensions().1,
            pitch_bytes: image.pitch_bytes(),
            format: PixelFormat::try_from(image.pixel_format())?,
            storage: MetalDeviceStorage::Resident { image },
        })
    }

    pub(crate) fn from_jpeg(surface: j2k_jpeg_metal::Surface) -> Result<Option<Self>, WsiError> {
        let Some(image) = surface.into_resident_metal_image() else {
            return Ok(None);
        };
        Self::from_resident(image).map(Some)
    }

    pub(crate) fn from_private_jpeg(
        tile: j2k_jpeg_metal::ResidentPrivateJpegTile,
    ) -> Result<Self, WsiError> {
        Self::from_resident(tile.into_resident_image())
    }

    pub(crate) fn from_j2k(surface: j2k_metal::Surface) -> Result<Option<Self>, WsiError> {
        let Some(image) = surface.into_resident_metal_image() else {
            return Ok(None);
        };
        Self::from_resident(image).map(Some)
    }

    pub(crate) fn crop_top_left(
        self,
        expected_width: u32,
        expected_height: u32,
    ) -> Result<Self, WsiError> {
        if expected_width == 0 || expected_height == 0 {
            return Ok(self);
        }
        let image = self.validated_resident_image()?;
        let dimensions = (
            image.dimensions().0.min(expected_width),
            image.dimensions().1.min(expected_height),
        );
        if dimensions == image.dimensions() {
            return Ok(self);
        }
        let layout = j2k_metal_support::MetalImageLayout::new(
            image.byte_offset(),
            dimensions,
            image.pitch_bytes(),
            image.pixel_format(),
        )
        .map_err(|source| interop::support_error("metal-tile-crop-layout", source))?;
        let cropped = image
            .view(layout)
            .map_err(|source| interop::support_error("metal-tile-crop-view", source))?;
        Self::from_resident(cropped)
    }

    pub(crate) fn ycbcr8_to_rgb8(
        &self,
        converter: &YcbcrToRgb8Converter,
    ) -> Result<Self, WsiError> {
        converter.convert_tile(self)
    }

    /// Validate the public compatibility metadata and borrow the resident image.
    ///
    /// Legacy raw-buffer storage is not a synchronized resident image and is
    /// rejected. Callers that own such a buffer must first use the explicit
    /// unsafe adoption API.
    #[allow(deprecated)]
    pub fn validated_resident_image(
        &self,
    ) -> Result<&j2k_metal_support::ResidentMetalImage, WsiError> {
        let image = match &self.storage {
            MetalDeviceStorage::Resident { image } => image,
            MetalDeviceStorage::Buffer { .. } => {
                return Err(WsiError::Unsupported {
                    reason: "legacy raw Metal buffer tiles must be explicitly adopted before resident access".into(),
                });
            }
        };
        let format = j2k_core::PixelFormat::from(self.format);
        if image.dimensions() != (self.width, self.height)
            || image.pitch_bytes() != self.pitch_bytes
            || image.pixel_format() != format
        {
            return Err(WsiError::Unsupported {
                reason:
                    "Metal device tile compatibility metadata does not match its resident image"
                        .into(),
            });
        }
        Ok(image)
    }

    pub(crate) fn resident_image_for_device(
        &self,
        device: &metal::DeviceRef,
    ) -> Result<&j2k_metal_support::ResidentMetalImage, WsiError> {
        let image = self.validated_resident_image()?;
        image
            .validate_device(device)
            .map_err(|source| interop::support_error("metal-resident-input-device", source))?;
        Ok(image)
    }
}
