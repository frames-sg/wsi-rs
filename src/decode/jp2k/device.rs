use super::prepare::prepare_jp2k_job;
use super::Jp2kDecodeJob;
use crate::error::WsiError;

#[cfg(feature = "metal")]
pub(crate) fn decode_batch_jp2k_metal(
    jobs: &[Jp2kDecodeJob<'_>],
    sessions: &crate::output::metal::MetalBackendSessions,
) -> Vec<Result<crate::output::metal::MetalDeviceTile, WsiError>> {
    if jobs.is_empty() {
        return Vec::new();
    }
    jobs.iter()
        .map(|job| {
            prepare_jp2k_job(job)
                .and_then(|job| super::metal_backend::decode_prepared_jp2k_metal(&job, sessions))
        })
        .collect()
}

#[cfg(feature = "cuda")]
pub(crate) fn decode_batch_jp2k_cuda(
    jobs: &[Jp2kDecodeJob<'_>],
    sessions: &crate::output::cuda::CudaBackendSessions,
) -> Vec<Result<crate::output::cuda::CudaDeviceTile, WsiError>> {
    jobs.iter()
        .map(|job| {
            prepare_jp2k_job(job)
                .and_then(|job| super::cuda::decode_prepared_jp2k_cuda(&job, sessions))
        })
        .collect()
}

#[cfg(all(test, feature = "metal"))]
pub(super) fn decode_one_jp2k_metal(
    job: &Jp2kDecodeJob<'_>,
    sessions: &crate::output::metal::MetalBackendSessions,
) -> Result<crate::output::metal::MetalDeviceTile, WsiError> {
    let prepared = prepare_jp2k_job(job)?;
    super::metal_backend::decode_prepared_jp2k_metal(&prepared, sessions)
}

#[cfg(all(test, feature = "cuda"))]
pub(super) fn decode_one_jp2k_cuda(
    job: &Jp2kDecodeJob<'_>,
    sessions: &crate::output::cuda::CudaBackendSessions,
) -> Result<crate::output::cuda::CudaDeviceTile, WsiError> {
    let prepared = prepare_jp2k_job(job)?;
    super::cuda::decode_prepared_jp2k_cuda(&prepared, sessions)
}
