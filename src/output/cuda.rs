use crate::{
    error::WsiError, ColorSpace, CpuTile, CpuTileData, CpuTileLayout, PixelFormat, SampleType,
};
use j2k_core::{BackendKind, DeviceSurface};
use std::sync::{Arc, Mutex};

/// Codec-specific CUDA sessions reused by compressed device decode paths.
#[derive(Debug, Clone)]
pub struct CudaBackendSessions {
    jpeg: Arc<Mutex<j2k_jpeg_cuda::CudaSession>>,
    j2k: Arc<Mutex<j2k_cuda::CudaSession>>,
}

impl CudaBackendSessions {
    pub fn new() -> Self {
        Self::from_sessions(
            j2k_jpeg_cuda::CudaSession::default(),
            j2k_cuda::CudaSession::default(),
        )
    }

    pub(crate) fn from_sessions(
        jpeg: j2k_jpeg_cuda::CudaSession,
        j2k: j2k_cuda::CudaSession,
    ) -> Self {
        Self {
            jpeg: Arc::new(Mutex::new(jpeg)),
            j2k: Arc::new(Mutex::new(j2k)),
        }
    }

    pub(crate) fn with_jpeg<R>(
        &self,
        decode: impl FnOnce(&mut j2k_jpeg_cuda::CudaSession) -> Result<R, WsiError>,
    ) -> Result<R, WsiError> {
        let mut session = self.jpeg.lock().map_err(|_| WsiError::Unsupported {
            reason: "CUDA JPEG session lock is poisoned".into(),
        })?;
        decode(&mut session)
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
        surface: Arc<j2k_jpeg_cuda::Surface>,
    },
    J2kSurface {
        surface: Arc<j2k_cuda::Surface>,
    },
}

impl CudaDeviceStorage {
    /// Borrow the J2k JPEG CUDA surface owner when this storage came from JPEG decode.
    pub fn jpeg_surface(&self) -> Option<&j2k_jpeg_cuda::Surface> {
        match self {
            Self::JpegSurface { surface } => Some(surface.as_ref()),
            Self::J2kSurface { .. } => None,
        }
    }

    /// Borrow the J2k J2K CUDA surface owner when this storage came from J2K decode.
    pub fn j2k_surface(&self) -> Option<&j2k_cuda::Surface> {
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
    /// Download this CUDA-resident tile into tightly packed CPU-owned storage.
    ///
    /// Surface pitch and CUDA allocation details remain internal to WSI-RS. The
    /// returned tile is validated, interleaved, and contains no row padding.
    pub fn download_cpu(&self) -> Result<CpuTile, WsiError> {
        self.validate_surface_metadata()?;
        let (row_bytes, byte_len) = tight_download_layout(self.width, self.height, self.format)?;
        let requested = u64::try_from(byte_len).unwrap_or(u64::MAX);
        if requested > crate::core::limits::MAX_DECODED_IMAGE_BYTES {
            return Err(WsiError::ResourceLimit {
                resource: "CUDA host tile download",
                requested,
                limit: crate::core::limits::MAX_DECODED_IMAGE_BYTES,
            });
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_len)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "CUDA host tile download",
                requested,
                limit: crate::core::limits::MAX_DECODED_IMAGE_BYTES,
            })?;
        bytes.resize(byte_len, 0);
        match &self.storage {
            CudaDeviceStorage::JpegSurface { surface } => surface
                .download_into(&mut bytes, row_bytes)
                .map_err(|source| WsiError::Codec {
                    codec: "cuda-jpeg-download",
                    source: Box::new(source),
                })?,
            CudaDeviceStorage::J2kSurface { surface } => surface
                .download_into(&mut bytes, row_bytes)
                .map_err(|source| WsiError::Codec {
                    codec: "cuda-j2k-download",
                    source: Box::new(source),
                })?,
        }
        downloaded_bytes_to_cpu_tile(self.width, self.height, self.format, bytes)
    }

    fn validate_surface_metadata(&self) -> Result<(), WsiError> {
        let (dimensions, pitch_bytes, format) = match &self.storage {
            CudaDeviceStorage::JpegSurface { surface } => (
                surface.dimensions(),
                surface.pitch_bytes(),
                PixelFormat::try_from(surface.pixel_format())?,
            ),
            CudaDeviceStorage::J2kSurface { surface } => (
                surface.dimensions(),
                surface.pitch_bytes(),
                PixelFormat::try_from(surface.pixel_format())?,
            ),
        };
        if dimensions != (self.width, self.height)
            || pitch_bytes != self.pitch_bytes
            || format != self.format
        {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "CUDA tile metadata does not match its surface: tile={}x{} {:?} pitch {}, surface={}x{} {:?} pitch {}",
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
        let (row_bytes, _) = tight_download_layout(self.width, self.height, self.format)?;
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

    pub(crate) fn from_jpeg(surface: j2k_jpeg_cuda::Surface) -> Result<Option<Self>, WsiError> {
        if surface.backend_kind() != BackendKind::Cuda {
            return Ok(None);
        }
        let Some(cuda_surface) = surface.cuda_surface() else {
            return Ok(None);
        };
        if cuda_surface.stats().decode_path() == j2k_jpeg_cuda::CudaJpegDecodePath::None {
            return Ok(None);
        }

        let dimensions = surface.dimensions();
        let pitch_bytes = surface.pitch_bytes();
        let format = PixelFormat::try_from(surface.pixel_format())?;
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

    pub(crate) fn from_j2k(surface: j2k_cuda::Surface) -> Result<Option<Self>, WsiError> {
        if surface.backend_kind() != BackendKind::Cuda
            || surface.residency() != j2k_cuda::SurfaceResidency::CudaResidentDecode
            || surface.cuda_surface().is_none()
        {
            return Ok(None);
        }

        let dimensions = surface.dimensions();
        let pitch_bytes = surface.pitch_bytes();
        let format = PixelFormat::try_from(surface.pixel_format())?;
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

fn tight_download_layout(
    width: u32,
    height: u32,
    format: PixelFormat,
) -> Result<(usize, usize), WsiError> {
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(format.bytes_per_pixel()))
        .ok_or_else(|| {
            WsiError::DisplayConversion("CUDA host download row size overflow".into())
        })?;
    let byte_len = usize::try_from(height)
        .ok()
        .and_then(|height| height.checked_mul(row_bytes))
        .ok_or_else(|| WsiError::DisplayConversion("CUDA host download size overflow".into()))?;
    Ok((row_bytes, byte_len))
}

fn downloaded_bytes_to_cpu_tile(
    width: u32,
    height: u32,
    format: PixelFormat,
    bytes: Vec<u8>,
) -> Result<CpuTile, WsiError> {
    let (_, expected) = tight_download_layout(width, height, format)?;
    if bytes.len() != expected {
        return Err(WsiError::DisplayConversion(format!(
            "CUDA host download expected {expected} bytes, received {}",
            bytes.len()
        )));
    }
    let data = match format.sample_type() {
        SampleType::Uint8 => CpuTileData::u8(bytes),
        SampleType::Uint16 => {
            let samples = bytes
                .chunks_exact(2)
                .map(|sample| u16::from_ne_bytes([sample[0], sample[1]]))
                .collect();
            CpuTileData::u16(samples)
        }
        SampleType::Float32 => {
            return Err(WsiError::Unsupported {
                reason: "CUDA decoded Float32 tiles are not supported".into(),
            });
        }
    };
    CpuTile::new(
        width,
        height,
        format.channels() as u16,
        match format.color_space() {
            ColorSpace::Rgb => ColorSpace::Rgb,
            ColorSpace::Rgba => ColorSpace::Rgba,
            ColorSpace::Grayscale => ColorSpace::Grayscale,
            _ => {
                return Err(WsiError::Unsupported {
                    reason: format!("CUDA pixel format {format:?} has an unsupported color space"),
                });
            }
        },
        CpuTileLayout::Interleaved,
        data,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColorSpace, CpuTileData, CpuTileLayout};

    #[test]
    fn tight_download_layout_rejects_dimension_overflow() {
        let error = tight_download_layout(u32::MAX, u32::MAX, PixelFormat::Rgba16)
            .expect_err("overflowing CUDA host download must fail before allocation");
        assert!(error.to_string().contains("overflow"), "{error}");
    }

    #[test]
    fn downloaded_bytes_convert_all_public_pixel_families() {
        for (format, channels, color_space) in [
            (PixelFormat::Gray8, 1, ColorSpace::Grayscale),
            (PixelFormat::Rgb8, 3, ColorSpace::Rgb),
            (PixelFormat::Rgba8, 4, ColorSpace::Rgba),
            (PixelFormat::Gray16, 1, ColorSpace::Grayscale),
            (PixelFormat::Rgb16, 3, ColorSpace::Rgb),
            (PixelFormat::Rgba16, 4, ColorSpace::Rgba),
        ] {
            let (_, byte_len) = tight_download_layout(2, 1, format).expect("valid layout");
            let bytes = (0..byte_len).map(|value| value as u8).collect();
            let tile = downloaded_bytes_to_cpu_tile(2, 1, format, bytes)
                .expect("downloaded bytes form a valid CpuTile");
            assert_eq!(tile.channels, channels);
            assert_eq!(tile.color_space, color_space);
            assert_eq!(tile.layout, CpuTileLayout::Interleaved);
            match (format.sample_type(), &tile.data) {
                (crate::SampleType::Uint8, CpuTileData::U8(samples)) => {
                    assert_eq!(samples.len(), 2 * channels as usize);
                }
                (crate::SampleType::Uint16, CpuTileData::U16(samples)) => {
                    assert_eq!(samples.len(), 2 * channels as usize);
                }
                other => panic!("unexpected CUDA CPU tile storage: {other:?}"),
            }
        }
    }

    #[test]
    fn downloaded_bytes_reject_undersized_output() {
        let error = downloaded_bytes_to_cpu_tile(2, 2, PixelFormat::Rgb8, vec![0; 11])
            .expect_err("undersized CUDA download must fail validation");
        assert!(error.to_string().contains("expected 12 bytes"), "{error}");
    }
}
