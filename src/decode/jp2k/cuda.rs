use j2k::CpuDecodeParallelism;
use j2k_core::{DeviceSurface as J2kDeviceSurface, PixelFormat as J2kPixelFormat};

use super::cpu::decode_prepared_jp2k_job;
use super::prepare::PreparedJp2kJob;
use super::Jp2kColorSpace;
use crate::core::types::{DeviceTile, TilePixels};
use crate::error::WsiError;
use crate::output::CudaBackendSessionsRef;

#[cfg(feature = "cuda")]
pub(super) fn decode_prepared_jp2k_pixels_cuda(
    job: &PreparedJp2kJob<'_>,
    require_device: bool,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    let Some(cuda_sessions) = cuda_sessions else {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for j2k without CUDA session".into(),
            });
        }
        return decode_prepared_jp2k_job(job, CpuDecodeParallelism::Auto).map(TilePixels::Cpu);
    };
    let surface = cuda_sessions.with_j2k(|session| {
        let mut decoder =
            j2k_cuda::J2kDecoder::new(job.input).map_err(|err| WsiError::Jp2k(err.to_string()))?;
        decoder
            .decode_to_device_with_session(J2kPixelFormat::Rgb8, session)
            .map_err(cuda_jp2k_decode_error)
    });

    match surface {
        Ok(surface) => match tile_pixels_from_cuda_jp2k_surface(
            surface,
            job.expected_width,
            job.expected_height,
            job.output_colorspace,
            require_device,
        ) {
            Ok(tile) => Ok(tile),
            Err(err) if require_device => Err(err),
            Err(_) => {
                decode_prepared_jp2k_job(job, CpuDecodeParallelism::Auto).map(TilePixels::Cpu)
            }
        },
        Err(err) if require_device => Err(err),
        Err(_) => decode_prepared_jp2k_job(job, CpuDecodeParallelism::Auto).map(TilePixels::Cpu),
    }
}

#[cfg(feature = "cuda")]
fn tile_pixels_from_cuda_jp2k_surface(
    surface: j2k_cuda::Surface,
    job_expected_width: u32,
    job_expected_height: u32,
    colorspace: Jp2kColorSpace,
    require_device: bool,
) -> Result<TilePixels, WsiError> {
    if surface.backend_kind() != j2k_core::BackendKind::Cuda {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for j2k".into(),
            });
        }
        let _ = (job_expected_width, job_expected_height, colorspace);
        return Err(WsiError::Unsupported {
            reason: "JP2K CUDA decode returned host surface".into(),
        });
    }
    if surface.residency() == j2k_cuda::SurfaceResidency::CpuStagedCudaUpload {
        if require_device {
            return Err(WsiError::Unsupported {
                reason:
                    "JP2K device decode produced CPU-staged CUDA upload instead of resident CUDA decode"
                        .into(),
            });
        }
        return Err(WsiError::Unsupported {
            reason: "JP2K CUDA decode produced CPU-staged CUDA upload".into(),
        });
    }
    if surface.residency() != j2k_cuda::SurfaceResidency::CudaResidentDecode
        || surface.cuda_surface().is_none()
    {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "JP2K CUDA decode did not return a resident CUDA surface".into(),
            });
        }
        let _ = (job_expected_width, job_expected_height, colorspace);
        return Err(WsiError::Unsupported {
            reason: "JP2K CUDA decode did not return a resident CUDA surface".into(),
        });
    }
    if colorspace == Jp2kColorSpace::YCbCr {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "JP2K CUDA YCbCr output requires resident CUDA RGB conversion, which wsi-rs does not own".into(),
            });
        }
        return Err(WsiError::Unsupported {
            reason: "JP2K CUDA YCbCr output requires CUDA RGB conversion".into(),
        });
    }
    if let Some(tile) = crate::output::cuda::CudaDeviceTile::from_j2k(surface)? {
        return Ok(TilePixels::Device(DeviceTile::Cuda(tile)));
    }
    if require_device {
        return Err(WsiError::Unsupported {
            reason: "device backend not available for j2k".into(),
        });
    }
    Err(WsiError::Unsupported {
        reason: "JP2K CUDA decode did not produce a public CUDA surface".into(),
    })
}

#[cfg(feature = "cuda")]
fn cuda_jp2k_decode_error(err: j2k_cuda::Error) -> WsiError {
    WsiError::Unsupported {
        reason: format!("JP2K CUDA device decode failed: {err}"),
    }
}
