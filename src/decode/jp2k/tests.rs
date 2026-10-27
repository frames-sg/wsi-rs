use super::batch::{
    materialize_jp2k_batch_outputs, try_decode_prepared_batch_jp2k_with_j2k, PreparedJp2kBatchJob,
};
#[cfg(feature = "cuda")]
use super::cpu::decode_one_jp2k_job_with_parallelism;
use super::prepare::prepare_jp2k_job;
use super::*;
use crate::core::types::CpuTile;
#[cfg(any(feature = "metal", feature = "cuda"))]
use crate::core::types::PixelFormat;
use crate::decode::jp2k_codestream::parse_codestream_header;
#[cfg(any(feature = "metal", feature = "cuda"))]
use crate::error::WsiError;
use crate::test_support::assert_cpu_tile_matches_rgb_fixture_with_tolerance;
use image::{DynamicImage, ImageFormat};
#[cfg(feature = "cuda")]
use j2k::CpuDecodeParallelism;
use std::io::Cursor;

pub(super) fn try_decode_batch_jp2k_with_j2k(jobs: &[Jp2kDecodeJob<'_>]) -> Option<Vec<CpuTile>> {
    let prepared = jobs
        .iter()
        .map(prepare_jp2k_job)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    try_decode_prepared_batch_jp2k_with_j2k(&prepared)
}

#[cfg(feature = "cuda")]
pub(super) fn decode_one_jp2k_job(job: &Jp2kDecodeJob<'_>) -> Result<CpuTile, WsiError> {
    decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Auto)
}

#[cfg(feature = "metal")]
pub(super) fn decode_one_jp2k_metal(
    job: &Jp2kDecodeJob<'_>,
    sessions: &crate::output::metal::MetalBackendSessions,
) -> Result<crate::output::metal::MetalDeviceTile, WsiError> {
    let prepared = prepare_jp2k_job(job)?;
    super::metal_backend::decode_prepared_jp2k_metal(&prepared, sessions)
}

#[cfg(feature = "cuda")]
pub(super) fn decode_one_jp2k_cuda(
    job: &Jp2kDecodeJob<'_>,
    sessions: &crate::output::cuda::CudaBackendSessions,
) -> Result<crate::output::cuda::CudaDeviceTile, WsiError> {
    let prepared = prepare_jp2k_job(job)?;
    super::cuda::decode_prepared_jp2k_cuda(&prepared, sessions)
}

fn load_fixture_rgb(ppm_bytes: &[u8]) -> image::RgbImage {
    match image::load(Cursor::new(ppm_bytes), ImageFormat::Pnm).unwrap() {
        DynamicImage::ImageRgb8(image) => image,
        other => other.to_rgb8(),
    }
}

const MAX_CHANNEL_DELTA: u8 = 4;
const MAX_AVG_CHANNEL_DELTA_X100: u64 = 100;

fn assert_sample_buffer_matches_rgb_fixture(image: &CpuTile, expected_rgb: &image::RgbImage) {
    assert_cpu_tile_matches_rgb_fixture_with_tolerance(
        image,
        expected_rgb,
        MAX_CHANNEL_DELTA,
        MAX_AVG_CHANNEL_DELTA_X100,
        "JP2K decode",
    );
}

#[path = "tests/batch.rs"]
mod batch;
#[cfg(all(feature = "metal", target_os = "macos"))]
#[path = "tests/cooperative.rs"]
mod cooperative;
#[path = "tests/cpu.rs"]
mod cpu;
#[cfg(any(feature = "metal", feature = "cuda", feature = "parity-metal"))]
#[path = "tests/device.rs"]
mod device;
#[path = "tests/errors.rs"]
mod errors;
