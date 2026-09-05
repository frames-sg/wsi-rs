use crate::output::download::{downloaded_bytes_to_cpu_tile, tight_download_layout};
use crate::{error::WsiError, CpuTile, PixelFormat};
use j2k_core::{BackendKind, DeviceSurface};
use std::sync::{Arc, Mutex};

/// CUDA session reused by JP2K and HTJ2K device decode paths.
#[derive(Debug, Clone)]
pub struct CudaBackendSessions {
    j2k: Arc<Mutex<j2k_cuda::CudaSession>>,
    device_identity: Arc<str>,
}

impl CudaBackendSessions {
    pub fn new() -> Self {
        Self::from_session_with_identity(j2k_cuda::CudaSession::default(), "cuda:auto")
    }

    pub(crate) fn system_default() -> Result<Self, WsiError> {
        let context = j2k_cuda_runtime::CudaContext::system_default().map_err(|source| {
            WsiError::Unsupported {
                reason: format!("CUDA JP2K acceleration unavailable: {source}"),
            }
        })?;
        let device_ordinal = context.device_ordinal();
        Ok(Self::from_session_for_device(
            j2k_cuda::CudaSession::with_context(context),
            device_ordinal,
        ))
    }

    pub(crate) fn from_session_for_device(
        j2k: j2k_cuda::CudaSession,
        device_ordinal: usize,
    ) -> Self {
        Self::from_session_with_identity(j2k, format!("cuda:{device_ordinal}"))
    }

    fn from_session_with_identity(
        j2k: j2k_cuda::CudaSession,
        device_identity: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            j2k: Arc::new(Mutex::new(j2k)),
            device_identity: device_identity.into(),
        }
    }

    pub(crate) fn with_j2k<R>(
        &self,
        decode: impl FnOnce(&mut j2k_cuda::CudaSession) -> Result<R, WsiError>,
    ) -> Result<R, WsiError> {
        let mut session = self.j2k.lock().map_err(|_| WsiError::Unsupported {
            reason: "CUDA J2K session lock is poisoned".into(),
        })?;
        decode(&mut session)
    }

    pub(crate) fn device_identity(&self) -> &str {
        &self.device_identity
    }
}

impl Default for CudaBackendSessions {
    fn default() -> Self {
        Self::new()
    }
}

/// CUDA-resident tile produced by strict JP2K or HTJ2K decode.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct CudaDeviceTile {
    pub width: u32,
    pub height: u32,
    pub pitch_bytes: usize,
    pub format: PixelFormat,
    pub storage: CudaDeviceStorage,
}

#[derive(Debug, Clone)]
pub struct CudaDeviceStorage {
    surface: Arc<j2k_cuda::Surface>,
}

impl CudaDeviceStorage {
    /// Borrow the resident J2K CUDA surface owner.
    pub fn j2k_surface(&self) -> &j2k_cuda::Surface {
        self.surface.as_ref()
    }

    /// Return the CUDA device pointer for the resident backing buffer.
    pub fn device_ptr(&self) -> u64 {
        self.surface
            .cuda_surface()
            .expect("CudaDeviceStorage must be CUDA-resident")
            .device_ptr()
    }

    /// Number of bytes in the backing surface allocation range exposed for this tile.
    pub fn byte_len(&self) -> usize {
        self.surface.byte_len()
    }
}

impl CudaDeviceTile {
    /// Download this CUDA-resident tile into tightly packed CPU-owned storage.
    ///
    /// Surface pitch and CUDA allocation details remain internal to WSI-RS. The
    /// returned tile is validated, interleaved, and contains no row padding.
    pub fn download_cpu(&self) -> Result<CpuTile, WsiError> {
        self.validate_surface_metadata()?;
        let (row_bytes, byte_len) =
            tight_download_layout(self.width, self.height, self.format, "CUDA")?;
        enforce_download_limit(byte_len)?;
        let requested = u64::try_from(byte_len).unwrap_or(u64::MAX);
        let surface = &self.storage.surface;
        let bytes = if surface.dimensions() == (self.width, self.height) {
            let mut bytes = try_download_buffer(byte_len, requested)?;
            surface
                .download_into(&mut bytes, row_bytes)
                .map_err(|source| WsiError::Codec {
                    codec: "cuda-j2k-download",
                    source: Box::new(source),
                })?;
            bytes
        } else {
            download_cropped_surface(surface, self.height, row_bytes)?
        };
        downloaded_bytes_to_cpu_tile(self.width, self.height, self.format, bytes, "CUDA")
    }

    fn validate_surface_metadata(&self) -> Result<(), WsiError> {
        let surface = &self.storage.surface;
        let dimensions = surface.dimensions();
        let pitch_bytes = surface.pitch_bytes();
        let format = PixelFormat::try_from(surface.pixel_format())?;
        if dimensions.0 < self.width
            || dimensions.1 < self.height
            || pitch_bytes != self.pitch_bytes
            || format != self.format
        {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "CUDA tile metadata is incompatible with its surface: tile={}x{} {:?} pitch {}, surface={}x{} {:?} pitch {}",
                    self.width,
                    self.height,
                    self.format,
                    self.pitch_bytes,
                    dimensions.0,
                    dimensions.1,
                    format,
                    pitch_bytes
                ),
            });
        }
        let (row_bytes, _) = tight_download_layout(self.width, self.height, self.format, "CUDA")?;
        if self.pitch_bytes < row_bytes {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "CUDA surface pitch {} is smaller than its {}-byte row",
                    self.pitch_bytes, row_bytes
                ),
            });
        }
        Ok(())
    }

    pub(crate) fn from_j2k(
        surface: j2k_cuda::Surface,
        expected_width: u32,
        expected_height: u32,
    ) -> Result<Option<Self>, WsiError> {
        if surface.backend_kind() != BackendKind::Cuda
            || surface.residency() != j2k_cuda::SurfaceResidency::CudaResidentDecode
            || surface.cuda_surface().is_none()
        {
            return Ok(None);
        }

        let dimensions = surface.dimensions();
        let width = if expected_width == 0 {
            dimensions.0
        } else {
            expected_width
        };
        let height = if expected_height == 0 {
            dimensions.1
        } else {
            expected_height
        };
        if width > dimensions.0 || height > dimensions.1 {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "CUDA logical tile dimensions {width}x{height} exceed decoded surface {}x{}",
                    dimensions.0, dimensions.1
                ),
            });
        }
        let pitch_bytes = surface.pitch_bytes();
        let format = PixelFormat::try_from(surface.pixel_format())?;
        Ok(Some(Self {
            width,
            height,
            pitch_bytes,
            format,
            storage: CudaDeviceStorage {
                surface: Arc::new(surface),
            },
        }))
    }
}

fn try_download_buffer(byte_len: usize, requested: u64) -> Result<Vec<u8>, WsiError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_len)
        .map_err(|_| WsiError::ResourceLimit {
            resource: "CUDA host tile download",
            requested,
            limit: MAX_DEVICE_DOWNLOAD_BYTES,
        })?;
    bytes.resize(byte_len, 0);
    Ok(bytes)
}

fn download_cropped_surface(
    surface: &j2k_cuda::Surface,
    logical_height: u32,
    logical_row_bytes: usize,
) -> Result<Vec<u8>, WsiError> {
    let format = PixelFormat::try_from(surface.pixel_format())?;
    let (physical_row_bytes, physical_len) = tight_download_layout(
        surface.dimensions().0,
        surface.dimensions().1,
        format,
        "CUDA",
    )?;
    enforce_download_limit(physical_len)?;
    let mut physical = try_download_buffer(
        physical_len,
        u64::try_from(physical_len).unwrap_or(u64::MAX),
    )?;
    surface
        .download_into(&mut physical, physical_row_bytes)
        .map_err(|source| WsiError::Codec {
            codec: "cuda-j2k-download",
            source: Box::new(source),
        })?;
    let logical_len = usize::try_from(logical_height)
        .ok()
        .and_then(|height| height.checked_mul(logical_row_bytes))
        .ok_or_else(|| WsiError::DisplayConversion("CUDA crop output size overflow".into()))?;
    let mut logical =
        try_download_buffer(logical_len, u64::try_from(logical_len).unwrap_or(u64::MAX))?;
    for (source, destination) in physical
        .chunks_exact(physical_row_bytes)
        .take(logical_height as usize)
        .zip(logical.chunks_exact_mut(logical_row_bytes))
    {
        destination.copy_from_slice(&source[..logical_row_bytes]);
    }
    Ok(logical)
}

const MAX_DEVICE_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;

fn enforce_download_limit(byte_len: usize) -> Result<(), WsiError> {
    let requested = u64::try_from(byte_len).unwrap_or(u64::MAX);
    if requested > MAX_DEVICE_DOWNLOAD_BYTES {
        return Err(WsiError::ResourceLimit {
            resource: "CUDA host tile download",
            requested,
            limit: MAX_DEVICE_DOWNLOAD_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
