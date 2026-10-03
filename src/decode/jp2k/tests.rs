use super::batch::{
    materialize_jp2k_batch_outputs, try_decode_prepared_batch_jp2k_with_j2k, PreparedJp2kBatchJob,
};
use super::cpu::decode_one_jp2k_job_with_parallelism;
use super::prepare::prepare_jp2k_job;
use super::*;
use crate::core::types::CpuTile;
#[cfg(any(feature = "metal", feature = "cuda"))]
use crate::core::types::PixelFormat;
use crate::decode::jp2k_codestream::parse_codestream_header;
use crate::error::WsiError;
use crate::test_support::assert_cpu_tile_matches_rgb_fixture_with_tolerance;
use image::{DynamicImage, ImageFormat, RgbaImage};
use j2k::CpuDecodeParallelism;
use std::io::Cursor;

pub(super) fn decode_jp2k_tile_batch_to_sample_buffers(
    reqs: &[Jp2kDecodeJob<'_>],
) -> Result<Vec<CpuTile>, WsiError> {
    if reqs.is_empty() {
        return Ok(Vec::new());
    }
    decode_jp2k_tile_batch_with_j2k(reqs)
}

pub(super) fn try_decode_batch_jp2k_with_j2k(jobs: &[Jp2kDecodeJob<'_>]) -> Option<Vec<CpuTile>> {
    let prepared = jobs
        .iter()
        .map(prepare_jp2k_job)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    try_decode_prepared_batch_jp2k_with_j2k(&prepared)
}

pub(super) fn decode_jp2k_tile_batch_with_j2k(
    reqs: &[Jp2kDecodeJob<'_>],
) -> Result<Vec<CpuTile>, WsiError> {
    reqs.iter().map(decode_one_jp2k_job).collect()
}

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

const MAX_CHANNEL_DELTA: u8 = 50;
const MAX_AVG_CHANNEL_DELTA_X100: u64 = 1600;

#[inline]
fn dimensions_from_bounds(x0: u32, x1: u32, y0: u32, y1: u32) -> Option<(u32, u32)> {
    Some((x1.checked_sub(x0)?, y1.checked_sub(y0)?))
}

/// Decode a raw JPEG2000 codestream (J2K, not JP2 container) into a
/// premultiplied RGBA image with alpha = 255.
fn decode_jp2k(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
) -> Result<RgbaImage, WsiError> {
    sample_buffer_to_rgba(decode_jp2k_to_sample_buffer(
        data,
        expected_width,
        expected_height,
        colorspace,
    )?)
}

fn sample_buffer_to_rgba(buffer: CpuTile) -> Result<RgbaImage, WsiError> {
    if buffer.channels != 3 || buffer.layout != crate::core::types::CpuTileLayout::Interleaved {
        return Err(WsiError::Jp2k(format!(
            "unsupported JP2K sample buffer layout for RGBA conversion: channels={}, layout={:?}",
            buffer.channels, buffer.layout
        )));
    }
    let rgb = buffer.data.as_u8().ok_or_else(|| {
        WsiError::Jp2k("unsupported JP2K sample data type for RGBA conversion".into())
    })?;
    let pixel_count = (buffer.width as usize)
        .checked_mul(buffer.height as usize)
        .ok_or_else(|| WsiError::Jp2k("JP2K RGBA image size overflow".into()))?;
    if rgb.len() != pixel_count * 3 {
        return Err(WsiError::Jp2k(format!(
            "unexpected JP2K RGB buffer length: expected {}, found {}",
            pixel_count * 3,
            rgb.len()
        )));
    }
    let mut rgba = vec![255u8; pixel_count * 4];
    for (src, dst) in rgb
        .as_chunks::<3>()
        .0
        .iter()
        .zip(rgba.as_chunks_mut::<4>().0)
    {
        dst[0] = src[0];
        dst[1] = src[1];
        dst[2] = src[2];
    }
    RgbaImage::from_raw(buffer.width, buffer.height, rgba)
        .ok_or_else(|| WsiError::Jp2k("failed to create RgbaImage from decoded JP2K data".into()))
}

fn assert_rgba_matches_rgb_fixture(decoded: &RgbaImage, expected_rgb: &image::RgbImage) {
    assert_eq!(decoded.width(), expected_rgb.width());
    assert_eq!(decoded.height(), expected_rgb.height());

    let mut total_delta = 0u64;
    let mut max_delta = 0u8;
    let mut channels = 0u64;

    for (decoded_pixel, expected_pixel) in decoded.pixels().zip(expected_rgb.pixels()) {
        for channel in 0..3 {
            let delta = decoded_pixel.0[channel].abs_diff(expected_pixel.0[channel]);
            total_delta += u64::from(delta);
            max_delta = max_delta.max(delta);
            channels += 1;
        }
        assert_eq!(decoded_pixel.0[3], 255);
    }

    let avg_delta_x100 = (total_delta * 100).checked_div(channels).unwrap_or(0);

    assert!(
        max_delta <= MAX_CHANNEL_DELTA,
        "JP2K decode drift too large: max channel delta {max_delta} > {MAX_CHANNEL_DELTA}",
    );
    assert!(
        avg_delta_x100 <= MAX_AVG_CHANNEL_DELTA_X100,
        "JP2K decode drift too large: average channel delta {:.2} > {:.2}",
        avg_delta_x100 as f64 / 100.0,
        MAX_AVG_CHANNEL_DELTA_X100 as f64 / 100.0,
    );
}

fn assert_sample_buffer_matches_rgb_fixture(image: &CpuTile, expected_rgb: &image::RgbImage) {
    assert_cpu_tile_matches_rgb_fixture_with_tolerance(
        image,
        expected_rgb,
        MAX_CHANNEL_DELTA,
        MAX_AVG_CHANNEL_DELTA_X100,
        "JP2K decode",
    );
}

fn assert_fixture_decodes_to_expected(
    codestream: &[u8],
    expected_ppm: &[u8],
    colorspace: Jp2kColorSpace,
) {
    let header = parse_codestream_header(codestream).unwrap();
    let expected = load_fixture_rgb(expected_ppm);
    let decoded = decode_jp2k(
        codestream,
        header.image_width,
        header.image_height,
        colorspace,
    )
    .unwrap();
    assert_rgba_matches_rgb_fixture(&decoded, &expected);
}

#[path = "tests/batch.rs"]
mod batch;
#[path = "tests/cpu.rs"]
mod cpu;
#[cfg(any(feature = "metal", feature = "cuda", feature = "parity-metal"))]
#[path = "tests/device.rs"]
mod device;
#[path = "tests/errors.rs"]
mod errors;
