use std::sync::{Arc, Mutex};

use crate::error::WsiError;
use objc2_metal::MTLDevice;

#[cfg(test)]
use super::MetalDeviceTile;
use super::{MetalDevice, YcbcrToRgb8Converter};

/// Metal session allocated for JP2K and HTJ2K device decode.
#[derive(Debug, Clone)]
pub struct MetalBackendSessions {
    pub(crate) j2k: Arc<j2k_metal::MetalBackendSession>,
    ycbcr_to_rgb8: Arc<Mutex<Option<Arc<YcbcrToRgb8Converter>>>>,
}

impl MetalBackendSessions {
    pub fn new(device: MetalDevice) -> Self {
        Self::from_session(j2k_metal::MetalBackendSession::new(device))
    }

    /// Create codec sessions on the system default Metal device.
    pub fn system_default() -> Result<Self, WsiError> {
        j2k_metal_support::system_default_device()
            .map(Self::new)
            .map_err(|source| super::interop::support_error("metal-session", source))
    }

    pub(crate) fn from_session(j2k: j2k_metal::MetalBackendSession) -> Self {
        Self {
            j2k: Arc::new(j2k),
            ycbcr_to_rgb8: Arc::new(Mutex::new(None)),
        }
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

    #[cfg(test)]
    pub(crate) fn ycbcr8_tiles_to_rgb8(
        &self,
        tiles: &[MetalDeviceTile],
    ) -> Result<Vec<MetalDeviceTile>, WsiError> {
        self.ycbcr_to_rgb8_converter()?.convert_tiles(tiles)
    }
}
