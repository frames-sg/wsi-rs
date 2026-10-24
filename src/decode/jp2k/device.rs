use super::prepare::prepare_jp2k_job;
use super::Jp2kDecodeJob;
use crate::core::types::TileRequest;
use crate::error::WsiError;

#[cfg(feature = "metal")]
pub(crate) fn decode_batch_jp2k_metal(
    jobs: &[Jp2kDecodeJob<'_>],
    sessions: &crate::output::metal::MetalBackendSessions,
) -> Vec<Result<crate::output::metal::MetalDeviceTile, WsiError>> {
    if jobs.is_empty() {
        return Vec::new();
    }
    #[cfg(target_os = "macos")]
    if jobs.len() > 1 {
        return super::metal_batch::decode_jobs(jobs, sessions);
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
    super::cuda::decode_prepared_jobs(jobs.iter().map(prepare_jp2k_job).collect(), sessions)
}

/// Pairs device decode results with their requests in order, naming the
/// request of the first failing tile.
pub(crate) fn map_tile_results<T>(
    results: Vec<Result<T, WsiError>>,
    reqs: &[TileRequest],
) -> Result<Vec<T>, WsiError> {
    results
        .into_iter()
        .zip(reqs)
        .map(|(result, request)| {
            result.map_err(|error| WsiError::tile_read(request, error.to_string()))
        })
        .collect()
}

/// Device batch windows bound both the image count and the decoded bytes. A
/// byte target alone admits more small images and makes native workspace
/// retention depend on that count; larger windows raised process RSS above the
/// 10% gate. This is an execution target, not an output limit: an individually
/// larger image still executes alone under the existing admission rules.
pub(crate) const DEVICE_WINDOW_MAX_IMAGES: usize = 16;
pub(crate) const DEVICE_WINDOW_TARGET_BYTES: u64 = 4 * 1024 * 1024;

/// Bytes one decoded image counts against a device window: 4 per pixel for
/// every backend, so Metal, CUDA and the adaptive router size windows alike.
pub(crate) fn device_window_bytes(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4)
}

/// One device batch window filled in decode order.
#[cfg(any(feature = "cuda", all(feature = "metal", target_os = "macos")))]
pub(super) struct DeviceWindow {
    images: usize,
    bytes: u64,
    target: u64,
}

#[cfg(any(feature = "cuda", all(feature = "metal", target_os = "macos")))]
impl DeviceWindow {
    pub(super) fn new(target: u64) -> Self {
        Self {
            images: 0,
            bytes: 0,
            target,
        }
    }

    /// Admits the next `width`x`height` image unless the window is full. The
    /// first image is always admitted, however large.
    pub(super) fn admit(&mut self, width: u32, height: u32) -> bool {
        let next = device_window_bytes(width, height);
        if self.images == DEVICE_WINDOW_MAX_IMAGES
            || (self.images > 0 && self.bytes.saturating_add(next) > self.target)
        {
            return false;
        }
        self.images += 1;
        self.bytes = self.bytes.saturating_add(next);
        true
    }
}
