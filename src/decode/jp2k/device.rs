use j2k::CpuDecodeParallelism;
#[cfg(feature = "cuda")]
use j2k_core::BackendRequest as J2kBackendRequest;

use super::cpu::decode_prepared_jp2k_job;
#[cfg(feature = "cuda")]
use super::cuda::decode_prepared_jp2k_pixels_cuda;
#[cfg(feature = "metal")]
use super::metal_backend::{
    decode_prepared_jp2k_pixels_metal, decode_prepared_jp2k_tile_batch_to_pixels,
};
use super::prepare::{prepare_jp2k_job, PreparedJp2kJob};
use super::Jp2kDecodeJob;
use crate::core::types::TilePixels;
use crate::error::WsiError;
use crate::output::{CudaBackendSessionsRef, MetalBackendSessionsRef};

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(crate) fn decode_batch_jp2k_pixels(
    jobs: &[Jp2kDecodeJob<'_>],
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Vec<Result<TilePixels, WsiError>> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let prepared = jobs.iter().map(prepare_jp2k_job).collect::<Vec<_>>();
    #[cfg(feature = "cuda")]
    if cuda_sessions.is_some()
        || jobs
            .iter()
            .any(|job| matches!(job.backend, J2kBackendRequest::Cuda))
    {
        return prepared
            .into_iter()
            .map(|job| {
                job.and_then(|job| {
                    decode_prepared_jp2k_pixels(&job, require_device, metal_sessions, cuda_sessions)
                })
            })
            .collect();
    }
    #[cfg(feature = "metal")]
    if prepared.iter().all(Result::is_ok) {
        let prepared_jobs = prepared
            .iter()
            .map(|job| *job.as_ref().expect("all preparation results checked"))
            .collect::<Vec<_>>();
        if let Ok(tiles) = decode_prepared_jp2k_tile_batch_to_pixels(
            &prepared_jobs,
            require_device,
            metal_sessions,
        ) {
            return tiles.into_iter().map(Ok).collect();
        }
    }

    prepared
        .into_iter()
        .map(|job| {
            job.and_then(|job| {
                decode_prepared_jp2k_pixels(&job, require_device, metal_sessions, cuda_sessions)
            })
        })
        .collect()
}

#[cfg(any(feature = "metal", feature = "cuda"))]
#[cfg(test)]
pub(super) fn decode_one_jp2k_pixels(
    job: &Jp2kDecodeJob<'_>,
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    let prepared = prepare_jp2k_job(job)?;
    decode_prepared_jp2k_pixels(&prepared, require_device, metal_sessions, cuda_sessions)
}

#[cfg(any(feature = "metal", feature = "cuda"))]
fn decode_prepared_jp2k_pixels(
    job: &PreparedJp2kJob<'_>,
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    #[cfg(not(feature = "metal"))]
    let _ = metal_sessions;
    #[cfg(not(feature = "cuda"))]
    let _ = cuda_sessions;
    #[cfg(feature = "cuda")]
    if cuda_sessions.is_some() || matches!(job.backend, J2kBackendRequest::Cuda) {
        return decode_prepared_jp2k_pixels_cuda(job, require_device, cuda_sessions);
    }

    #[cfg(feature = "metal")]
    {
        return decode_prepared_jp2k_pixels_metal(job, require_device, metal_sessions);
    }

    #[allow(unreachable_code)]
    if require_device {
        Err(WsiError::Unsupported {
            reason: "device backend not available for j2k".into(),
        })
    } else {
        decode_prepared_jp2k_job(job, CpuDecodeParallelism::Auto).map(TilePixels::Cpu)
    }
}
