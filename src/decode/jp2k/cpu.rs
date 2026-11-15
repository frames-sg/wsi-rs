use j2k::{
    CpuDecodeParallelism, J2kDecoder as J2kJp2kDecoder, J2kReferenceGrid, J2kScratchPool, J2kView,
};
use j2k_core::{BackendRequest as J2kBackendRequest, PixelFormat as J2kPixelFormat, Rect};

use super::output::sample_buffer_from_rgb8_bytes;
use super::prepare::{prepare_jp2k_input_and_view, PreparedJp2kJob};
use super::Jp2kColorSpace;
use super::Jp2kDecodeJob;
use crate::core::types::CpuTile;
use crate::decode::jp2k_backend::effective_output_colorspace;
use crate::decode::jp2k_codestream::{
    codestream_header_from_view, validate_component_contract, validate_decoded_size,
};
use crate::error::WsiError;

pub(crate) fn decode_jp2k_to_sample_buffer(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
) -> Result<CpuTile, WsiError> {
    decode_jp2k_to_sample_buffer_with_backend(
        data,
        expected_width,
        expected_height,
        colorspace,
        J2kBackendRequest::Auto,
    )
}

/// Decodes `region`, in full-resolution coordinates, from the codestream's own
/// reduced-resolution image. Each reduction level discards one wavelet level;
/// nothing is resampled from a full decode. Only the region's output is
/// bounded, so this reads regions of codestreams too large to decode whole. A
/// `Serial` decode runs Tier-1 in bands as the inverse DWT reads them, holding
/// one band of coefficients per sub-band.
pub(crate) fn decode_jp2k_region_to_sample_buffer(
    data: &[u8],
    region: Rect,
    reduction_levels: u8,
    parallelism: CpuDecodeParallelism,
    colorspace: Jp2kColorSpace,
) -> Result<CpuTile, WsiError> {
    let view = J2kView::parse(data).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    let header = codestream_header_from_view(&view)?;
    validate_component_contract(&header)?;
    let (width, height) =
        reduced_jp2k_region_dimensions(&view.reference_grid(), region, reduction_levels)?;
    validate_decoded_size(width, height)?;
    let row_bytes = width as usize * J2kPixelFormat::Rgb8.bytes_per_pixel();
    let mut rgb = vec![0; row_bytes * height as usize];
    let mut decoder =
        J2kJp2kDecoder::from_view(view).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    decoder.set_cpu_decode_parallelism(parallelism);
    decoder
        .decode_region_scaled_pow2_into(
            &mut J2kScratchPool::default(),
            &mut rgb,
            row_bytes,
            J2kPixelFormat::Rgb8,
            region,
            reduction_levels,
        )
        .map_err(|err| WsiError::Jp2k(format!("j2k JP2K region decode failed: {err}")))?;
    sample_buffer_from_rgb8_bytes(
        rgb,
        width,
        height,
        width,
        height,
        effective_output_colorspace(&header, colorspace),
    )
}

/// Dimensions of the whole image at `reduction_levels`, on the codestream's
/// reference grid, image origin included.
pub(crate) fn reduced_jp2k_dimensions(
    grid: &J2kReferenceGrid,
    reduction_levels: u8,
) -> Result<(u32, u32), WsiError> {
    let (w, h) = grid.image_size;
    reduced_jp2k_region_dimensions(grid, Rect { x: 0, y: 0, w, h }, reduction_levels)
}

/// The reduced samples a decode of `region`, in full-resolution coordinates,
/// covers at `reduction_levels`, as j2k decodes it.
pub(super) fn reduced_jp2k_region_dimensions(
    grid: &J2kReferenceGrid,
    region: Rect,
    reduction_levels: u8,
) -> Result<(u32, u32), WsiError> {
    grid.reduced_region(region, reduction_levels)
        .map(|reduced| (reduced.w, reduced.h))
        .ok_or_else(|| {
            WsiError::Jp2k(format!(
                "JP2K region {}x{} at ({}, {}) is empty, leaves the {}x{} image, or has unrepresentable reduction {reduction_levels}",
                region.w, region.h, region.x, region.y, grid.image_size.0, grid.image_size.1,
            ))
        })
}

/// The deepest reduction every component and tile of this codestream decodes.
/// j2k reports the ladder after tile and component coding-style overrides
/// shorten the main header's. A level whose reduced image holds no samples,
/// which a small image at an odd reference-grid origin can reach, is not
/// offered. The top-left reduced sample is then decoded at that depth, so a
/// codestream whose packets cannot be read fails when it is opened.
pub(crate) fn jp2k_decodable_reduction_levels(data: &[u8]) -> Result<u8, WsiError> {
    let probe_error =
        |error: j2k::J2kError| WsiError::Jp2k(format!("j2k JP2K reduction probe failed: {error}"));
    let mut decoder = J2kJp2kDecoder::new(data).map_err(probe_error)?;
    let grid = decoder.reference_grid();
    let mut levels = decoder.max_reduction_levels().map_err(probe_error)?;
    while levels > 0 && matches!(reduced_jp2k_dimensions(&grid, levels)?, (0, _) | (_, 0)) {
        levels -= 1;
    }
    if levels == 0 {
        return Ok(0);
    }
    // One full-resolution block of 2^levels per side, clamped to the image,
    // spans exactly one reduced sample whatever the origin; a single pixel
    // at (0, 0) covers none when the origin is not a multiple of 2^levels.
    let side = 1_u32 << levels;
    let probe = Rect {
        x: 0,
        y: 0,
        w: grid.image_size.0.min(side),
        h: grid.image_size.1.min(side),
    };
    let (width, height) = reduced_jp2k_region_dimensions(&grid, probe, levels)?;
    let row_bytes = width as usize * J2kPixelFormat::Rgb8.bytes_per_pixel();
    let mut pixels = vec![0_u8; row_bytes * height as usize];
    decoder.set_cpu_decode_parallelism(CpuDecodeParallelism::Serial);
    decoder
        .decode_region_scaled_pow2_into(
            &mut J2kScratchPool::default(),
            &mut pixels,
            row_bytes,
            J2kPixelFormat::Rgb8,
            probe,
            levels,
        )
        .map_err(probe_error)?;
    Ok(levels)
}

fn decode_jp2k_to_sample_buffer_with_backend(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
    backend: J2kBackendRequest,
) -> Result<CpuTile, WsiError> {
    decode_jp2k_to_sample_buffer_with_backend_and_parallelism(
        data,
        expected_width,
        expected_height,
        colorspace,
        backend,
        CpuDecodeParallelism::Auto,
    )
}

fn decode_jp2k_to_sample_buffer_with_backend_and_parallelism(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
    backend: J2kBackendRequest,
    parallelism: CpuDecodeParallelism,
) -> Result<CpuTile, WsiError> {
    let (prepared, view) =
        prepare_jp2k_input_and_view(data, expected_width, expected_height, colorspace, backend)?;
    if !matches!(backend, J2kBackendRequest::Auto | J2kBackendRequest::Cpu) {
        return Err(WsiError::Unsupported {
            reason: "device backend not available for CPU JP2K sample-buffer decode".into(),
        });
    }
    let decoder =
        J2kJp2kDecoder::from_view(view).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    decode_with_decoder(&prepared, parallelism, decoder)
}

pub(super) fn decode_prepared_jp2k_job(
    prepared: &PreparedJp2kJob<'_>,
    parallelism: CpuDecodeParallelism,
) -> Result<CpuTile, WsiError> {
    match prepared.backend {
        J2kBackendRequest::Auto | J2kBackendRequest::Cpu => {
            decode_jp2k_to_sample_buffer_cpu(prepared, parallelism)
        }
        J2kBackendRequest::Metal | J2kBackendRequest::Cuda => Err(WsiError::Unsupported {
            reason: "device backend not available for CPU JP2K sample-buffer decode".into(),
        }),
    }
}

pub(super) fn decode_one_jp2k_job_with_parallelism(
    job: &Jp2kDecodeJob<'_>,
    parallelism: CpuDecodeParallelism,
) -> Result<CpuTile, WsiError> {
    decode_jp2k_to_sample_buffer_with_backend_and_parallelism(
        job.data.as_ref(),
        job.expected_width,
        job.expected_height,
        if job.rgb_color_space {
            Jp2kColorSpace::Rgb
        } else {
            Jp2kColorSpace::YCbCr
        },
        job.backend,
        parallelism,
    )
    .map_err(|err| WsiError::Codec {
        codec: "j2k",
        source: Box::new(err),
    })
}

fn decode_jp2k_to_sample_buffer_cpu(
    prepared: &PreparedJp2kJob<'_>,
    parallelism: CpuDecodeParallelism,
) -> Result<CpuTile, WsiError> {
    let decoder =
        J2kJp2kDecoder::new(prepared.input).map_err(|err| WsiError::Jp2k(err.to_string()))?;
    decode_with_decoder(prepared, parallelism, decoder)
}

fn decode_with_decoder(
    prepared: &PreparedJp2kJob<'_>,
    parallelism: CpuDecodeParallelism,
    mut decoder: J2kJp2kDecoder<'_>,
) -> Result<CpuTile, WsiError> {
    decoder.set_cpu_decode_parallelism(parallelism);
    let mut rgb = vec![0; prepared.output_len];

    decoder
        .decode_into(&mut rgb, prepared.row_bytes, J2kPixelFormat::Rgb8)
        .map_err(|err| WsiError::Jp2k(format!("j2k JP2K decode failed: {err}")))?;

    sample_buffer_from_rgb8_bytes(
        rgb,
        prepared.decoded_width,
        prepared.decoded_height,
        prepared.expected_width,
        prepared.expected_height,
        prepared.output_colorspace,
    )
}
