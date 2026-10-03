use j2k::{CpuDecodeParallelism, J2kDecoder as J2kJp2kDecoder, J2kScratchPool, J2kView};
use j2k_core::{BackendRequest as J2kBackendRequest, PixelFormat as J2kPixelFormat, Rect};

use super::output::sample_buffer_from_rgb8_bytes;
use super::prepare::{prepare_jp2k_input_and_view, PreparedJp2kJob};
use super::Jp2kColorSpace;
use super::Jp2kDecodeJob;
use crate::core::types::CpuTile;
use crate::decode::jp2k_backend::effective_output_colorspace;
use crate::decode::jp2k_codestream::{codestream_header_from_view, validate_pixel_contract};
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

/// Decodes the codestream's own reduced-resolution image. Each reduction level
/// discards one wavelet level; nothing is resampled from a full decode.
pub(crate) fn decode_jp2k_reduced_to_sample_buffer(
    data: &[u8],
    reduction_levels: u8,
    colorspace: Jp2kColorSpace,
) -> Result<CpuTile, WsiError> {
    let view = J2kView::parse(data).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    let header = codestream_header_from_view(&view)?;
    validate_pixel_contract(&header)?;
    let full = (header.image_width, header.image_height);
    let (width, height) = reduced_jp2k_dimensions(full, reduction_levels)?;
    let row_bytes = width as usize * J2kPixelFormat::Rgb8.bytes_per_pixel();
    let mut rgb = vec![0; row_bytes * height as usize];
    let mut decoder =
        J2kJp2kDecoder::from_view(view).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    decoder.set_cpu_decode_parallelism(CpuDecodeParallelism::Auto);
    decoder
        .decode_region_scaled_pow2_into(
            &mut J2kScratchPool::default(),
            &mut rgb,
            row_bytes,
            J2kPixelFormat::Rgb8,
            Rect::full(full),
            reduction_levels,
        )
        .map_err(|err| WsiError::Jp2k(format!("j2k JP2K reduced decode failed: {err}")))?;
    sample_buffer_from_rgb8_bytes(
        rgb,
        width,
        height,
        width,
        height,
        effective_output_colorspace(&header, colorspace),
    )
}

/// Reduced dimensions follow the codestream grid: each level rounds up.
pub(crate) fn reduced_jp2k_dimensions(
    (width, height): (u32, u32),
    reduction_levels: u8,
) -> Result<(u32, u32), WsiError> {
    let denominator = 1_u32
        .checked_shl(u32::from(reduction_levels))
        .ok_or_else(|| {
            WsiError::Jp2k(format!("unrepresentable JP2K reduction {reduction_levels}"))
        })?;
    Ok((width.div_ceil(denominator), height.div_ceil(denominator)))
}

/// The deepest reduction this codestream decodes. The main header advertises a
/// resolution ladder that component overrides may shorten, so each candidate
/// depth is proven with a one-pixel decode before it is offered.
pub(crate) fn jp2k_decodable_reduction_levels(data: &[u8]) -> Result<u8, WsiError> {
    let view = J2kView::parse(data).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    let advertised = view.info().resolution_levels.saturating_sub(1);
    let mut decoder =
        J2kJp2kDecoder::from_view(view).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    decoder.set_cpu_decode_parallelism(CpuDecodeParallelism::Serial);
    let mut pool = J2kScratchPool::default();
    let mut pixel = [0_u8; 3];
    let stride = pixel.len();
    let origin = Rect {
        x: 0,
        y: 0,
        w: 1,
        h: 1,
    };
    for levels in (1..=advertised).rev() {
        match decoder.decode_region_scaled_pow2_into(
            &mut pool,
            &mut pixel,
            stride,
            J2kPixelFormat::Rgb8,
            origin,
            levels,
        ) {
            Ok(_) => return Ok(levels),
            Err(j2k::J2kError::Unsupported(j2k_core::Unsupported { what }))
                if matches!(
                    what,
                    "requested reduction exceeds the codestream resolution ladder"
                        | "tile coding style has fewer levels than the requested reduction"
                        | "requested reduction exceeds supported image geometry"
                        | "native backend did not honor the requested reduction level"
                ) =>
            {
                tracing::debug!(what, levels, "JP2K reduction is not decodable");
            }
            Err(error) => {
                return Err(WsiError::Jp2k(format!(
                    "j2k JP2K reduction probe failed: {error}"
                )));
            }
        }
    }
    Ok(0)
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
