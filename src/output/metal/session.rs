use std::sync::{Arc, Mutex};

use crate::error::WsiError;
use objc2_metal::MTLDevice;

use super::{MetalDevice, MetalDeviceTile, YcbcrToRgb8Converter};

/// Codec-specific Metal sessions allocated from one renderer-owned device.
#[derive(Debug, Clone)]
pub struct MetalBackendSessions {
    pub(crate) jpeg: Arc<j2k_jpeg_metal::MetalBackendSession>,
    pub(crate) j2k: Arc<j2k_metal::MetalBackendSession>,
    ycbcr_to_rgb8: Arc<Mutex<Option<Arc<YcbcrToRgb8Converter>>>>,
    private_jpeg_decode: bool,
}

impl MetalBackendSessions {
    pub fn new(device: MetalDevice) -> Self {
        Self::from_sessions(
            j2k_jpeg_metal::MetalBackendSession::new(device.clone()),
            j2k_metal::MetalBackendSession::new(device),
        )
    }

    /// Create codec sessions on the system default Metal device.
    pub fn system_default() -> Result<Self, WsiError> {
        j2k_metal_support::system_default_device()
            .map(Self::new)
            .map_err(|source| super::interop::support_error("metal-session", source))
    }

    pub(crate) fn from_sessions(
        jpeg: j2k_jpeg_metal::MetalBackendSession,
        j2k: j2k_metal::MetalBackendSession,
    ) -> Self {
        Self {
            jpeg: Arc::new(jpeg),
            j2k: Arc::new(j2k),
            ycbcr_to_rgb8: Arc::new(Mutex::new(None)),
            private_jpeg_decode: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_private_jpeg_decode(mut self) -> Self {
        self.private_jpeg_decode = true;
        self
    }

    pub(crate) fn jpeg(&self) -> &j2k_jpeg_metal::MetalBackendSession {
        &self.jpeg
    }

    pub(crate) fn private_jpeg_decode(&self) -> bool {
        self.private_jpeg_decode
    }

    pub(crate) fn j2k(&self) -> &j2k_metal::MetalBackendSession {
        &self.j2k
    }

    pub(crate) fn device_identity(&self) -> String {
        #[cfg(target_os = "macos")]
        {
            self.j2k.device().name().to_string()
        }
        #[cfg(not(target_os = "macos"))]
        {
            "metal".to_string()
        }
    }

    pub(crate) fn ycbcr_to_rgb8_converter(&self) -> Result<Arc<YcbcrToRgb8Converter>, WsiError> {
        let mut cached = self
            .ycbcr_to_rgb8
            .lock()
            .map_err(|_| WsiError::Unsupported {
                reason: "Metal YCbCr converter cache lock is poisoned".into(),
            })?;
        if let Some(converter) = cached.as_ref() {
            return Ok(converter.clone());
        }

        let converter = Arc::new(YcbcrToRgb8Converter::new(self.j2k())?);
        *cached = Some(converter.clone());
        Ok(converter)
    }

    pub(crate) fn ycbcr8_tiles_to_rgb8(
        &self,
        tiles: &[MetalDeviceTile],
    ) -> Result<Vec<MetalDeviceTile>, WsiError> {
        self.ycbcr_to_rgb8_converter()?.convert_tiles(tiles)
    }
}
