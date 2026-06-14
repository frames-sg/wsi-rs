use crate::{error::WsiError, PixelFormat};
use signinum_core::{BackendKind, DeviceSurface};
use std::sync::{Arc, Mutex};

/// Codec-specific CUDA sessions reused by compressed device decode paths.
#[derive(Debug, Clone)]
pub struct CudaBackendSessions {
    jpeg: Arc<Mutex<signinum_jpeg_cuda::CudaSession>>,
    j2k: Arc<Mutex<signinum_j2k_cuda::CudaSession>>,
}

impl CudaBackendSessions {
    pub fn new() -> Self {
        Self::from_sessions(
            signinum_jpeg_cuda::CudaSession::default(),
            signinum_j2k_cuda::CudaSession::default(),
        )
    }

    pub(crate) fn from_sessions(
        jpeg: signinum_jpeg_cuda::CudaSession,
        j2k: signinum_j2k_cuda::CudaSession,
    ) -> Self {
        Self {
            jpeg: Arc::new(Mutex::new(jpeg)),
            j2k: Arc::new(Mutex::new(j2k)),
        }
    }

    pub(crate) fn with_jpeg<R>(
        &self,
        decode: impl FnOnce(&mut signinum_jpeg_cuda::CudaSession) -> Result<R, WsiError>,
    ) -> Result<R, WsiError> {
        let mut session = self.jpeg.lock().map_err(|_| WsiError::Unsupported {
            reason: "CUDA JPEG session lock is poisoned".into(),
        })?;
        decode(&mut session)
    }

    pub(crate) fn with_j2k<R>(
        &self,
        decode: impl FnOnce(&mut signinum_j2k_cuda::CudaSession) -> Result<R, WsiError>,
    ) -> Result<R, WsiError> {
        let mut session = self.j2k.lock().map_err(|_| WsiError::Unsupported {
            reason: "CUDA J2K session lock is poisoned".into(),
        })?;
        decode(&mut session)
    }

    pub(crate) fn device_identity(&self) -> String {
        "cuda".to_string()
    }
}

impl Default for CudaBackendSessions {
    fn default() -> Self {
        Self::new()
    }
}

/// CUDA-backed device tile returned from `TilePixels::Device`.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct CudaDeviceTile {
    pub width: u32,
    pub height: u32,
    pub pitch_bytes: usize,
    pub format: PixelFormat,
    pub storage: CudaDeviceStorage,
}

/// Concrete CUDA storage backing a [`CudaDeviceTile`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum CudaDeviceStorage {
    JpegSurface {
        surface: Arc<signinum_jpeg_cuda::Surface>,
    },
    J2kSurface {
        surface: Arc<signinum_j2k_cuda::Surface>,
    },
}

impl CudaDeviceStorage {
    /// Borrow the Signinum JPEG CUDA surface owner when this storage came from JPEG decode.
    pub fn jpeg_surface(&self) -> Option<&signinum_jpeg_cuda::Surface> {
        match self {
            Self::JpegSurface { surface } => Some(surface.as_ref()),
            Self::J2kSurface { .. } => None,
        }
    }

    /// Borrow the Signinum J2K CUDA surface owner when this storage came from J2K decode.
    pub fn j2k_surface(&self) -> Option<&signinum_j2k_cuda::Surface> {
        match self {
            Self::JpegSurface { .. } => None,
            Self::J2kSurface { surface } => Some(surface.as_ref()),
        }
    }

    /// Return the CUDA device pointer for the resident backing buffer.
    pub fn device_ptr(&self) -> u64 {
        match self {
            Self::JpegSurface { surface } => surface
                .cuda_surface()
                .expect("CudaDeviceStorage::JpegSurface must be CUDA-resident")
                .device_ptr(),
            Self::J2kSurface { surface } => surface
                .cuda_surface()
                .expect("CudaDeviceStorage::J2kSurface must be CUDA-resident")
                .device_ptr(),
        }
    }

    /// Number of bytes in the backing surface allocation range exposed for this tile.
    pub fn byte_len(&self) -> usize {
        match self {
            Self::JpegSurface { surface } => surface.byte_len(),
            Self::J2kSurface { surface } => surface.byte_len(),
        }
    }
}

impl CudaDeviceTile {
    pub(crate) fn from_jpeg(
        surface: signinum_jpeg_cuda::Surface,
    ) -> Result<Option<Self>, WsiError> {
        if surface.backend_kind() != BackendKind::Cuda {
            return Ok(None);
        }
        let Some(cuda_surface) = surface.cuda_surface() else {
            return Ok(None);
        };
        if cuda_surface.stats().decode_path() == signinum_jpeg_cuda::CudaJpegDecodePath::None {
            return Ok(None);
        }

        let dimensions = surface.dimensions();
        let pitch_bytes = surface.pitch_bytes();
        let format = PixelFormat::try_from_signinum(surface.pixel_format())?;
        Ok(Some(Self {
            width: dimensions.0,
            height: dimensions.1,
            pitch_bytes,
            format,
            storage: CudaDeviceStorage::JpegSurface {
                surface: Arc::new(surface),
            },
        }))
    }

    pub(crate) fn from_j2k(surface: signinum_j2k_cuda::Surface) -> Result<Option<Self>, WsiError> {
        if surface.backend_kind() != BackendKind::Cuda
            || surface.residency() != signinum_j2k_cuda::SurfaceResidency::CudaResidentDecode
            || surface.cuda_surface().is_none()
        {
            return Ok(None);
        }

        let dimensions = surface.dimensions();
        let pitch_bytes = surface.pitch_bytes();
        let format = PixelFormat::try_from_signinum(surface.pixel_format())?;
        Ok(Some(Self {
            width: dimensions.0,
            height: dimensions.1,
            pitch_bytes,
            format,
            storage: CudaDeviceStorage::J2kSurface {
                surface: Arc::new(surface),
            },
        }))
    }
}
