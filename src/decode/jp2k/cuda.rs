use j2k_core::{DeviceSurface as J2kDeviceSurface, PixelFormat as J2kPixelFormat};

use super::prepare::PreparedJp2kJob;
use super::Jp2kColorSpace;
use crate::error::WsiError;

pub(super) fn decode_prepared_jp2k_cuda(
    job: &PreparedJp2kJob<'_>,
    sessions: &crate::output::cuda::CudaBackendSessions,
) -> Result<crate::output::cuda::CudaDeviceTile, WsiError> {
    let surface = sessions.with_j2k(|session| {
        let mut decoder =
            j2k_cuda::J2kDecoder::new(job.input).map_err(|err| WsiError::Jp2k(err.to_string()))?;
        decoder
            .decode_to_device_with_session(J2kPixelFormat::Rgb8, session)
            .map_err(cuda_jp2k_decode_error)
    })?;
    cuda_tile_from_jp2k_surface(
        surface,
        job.expected_width,
        job.expected_height,
        job.output_colorspace,
    )
}

fn cuda_tile_from_jp2k_surface(
    surface: j2k_cuda::Surface,
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
) -> Result<crate::output::cuda::CudaDeviceTile, WsiError> {
    if surface.backend_kind() != j2k_core::BackendKind::Cuda {
        return Err(WsiError::Unsupported {
            reason: "strict JP2K CUDA decode returned a host surface".into(),
        });
    }
    if surface.residency() != j2k_cuda::SurfaceResidency::CudaResidentDecode
        || surface.cuda_surface().is_none()
    {
        return Err(WsiError::Unsupported {
            reason: "strict JP2K CUDA decode did not return a resident CUDA surface".into(),
        });
    }
    if colorspace == Jp2kColorSpace::YCbCr {
        return Err(WsiError::Unsupported {
            reason: "strict JP2K CUDA YCbCr output requires a resident CUDA RGB conversion".into(),
        });
    }
    crate::output::cuda::CudaDeviceTile::from_j2k(surface, expected_width, expected_height)?
        .ok_or_else(|| WsiError::Unsupported {
            reason: "strict JP2K CUDA decode did not produce a public resident tile".into(),
        })
}

fn cuda_jp2k_decode_error(err: j2k_cuda::Error) -> WsiError {
    WsiError::Unsupported {
        reason: format!("strict JP2K CUDA device decode failed: {err}"),
    }
}
