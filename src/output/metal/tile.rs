use crate::output::download::{downloaded_bytes_to_cpu_tile, tight_download_layout};
use crate::{error::WsiError, CpuTile, PixelFormat};
use objc2::runtime::ProtocolObject;
use objc2_metal::MTLDevice;

use super::{interop, YcbcrToRgb8Converter};

/// Metal-resident tile produced by strict JP2K or HTJ2K decode.
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

    /// Download this Metal-resident tile into tightly packed CPU-owned storage.
    ///
    /// The readback copies only logical row bytes, so cropped views and
    /// padded Metal rows do not leak padding into the returned tile.
    pub fn download_cpu(&self) -> Result<CpuTile, WsiError> {
        let image = self.validated_resident_image()?;
        let (row_bytes, byte_len) =
            tight_download_layout(self.width, self.height, self.format, "Metal")?;
        enforce_download_limit(byte_len)?;
        if self.pitch_bytes < row_bytes {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "Metal surface pitch {} is smaller than its {}-byte row",
                    self.pitch_bytes, row_bytes
                ),
            });
        }
        let bytes = interop::download_resident_rows(image, row_bytes, byte_len)?;
        downloaded_bytes_to_cpu_tile(self.width, self.height, self.format, bytes, "Metal")
    }

    /// Validate the public compatibility metadata and borrow the resident image.
    ///
    pub fn validated_resident_image(
        &self,
    ) -> Result<&j2k_metal_support::ResidentMetalImage, WsiError> {
        let image = match &self.storage {
            MetalDeviceStorage::Resident { image } => image,
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
        device: &ProtocolObject<dyn MTLDevice>,
    ) -> Result<&j2k_metal_support::ResidentMetalImage, WsiError> {
        let image = self.validated_resident_image()?;
        image
            .validate_device(device)
            .map_err(|source| interop::support_error("metal-resident-input-device", source))?;
        Ok(image)
    }
}

pub(super) const MAX_DEVICE_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;

pub(super) fn enforce_download_limit(byte_len: usize) -> Result<(), WsiError> {
    let requested = u64::try_from(byte_len).unwrap_or(u64::MAX);
    if requested > MAX_DEVICE_DOWNLOAD_BYTES {
        return Err(WsiError::ResourceLimit {
            resource: "Metal host tile download",
            requested,
            limit: MAX_DEVICE_DOWNLOAD_BYTES,
        });
    }
    Ok(())
}
