use j2k::{CpuDecodeParallelism, J2kDecoder as J2kJp2kDecoder};
use j2k_core::{BackendRequest as J2kBackendRequest, PixelFormat as J2kPixelFormat};

use super::output::sample_buffer_from_rgb8_bytes;
use super::prepare::{prepare_jp2k_input_and_view, PreparedJp2kJob};
use super::Jp2kColorSpace;
use super::Jp2kDecodeJob;
use crate::core::types::CpuTile;
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

#[cfg(test)]
pub(super) fn decode_one_jp2k_job(job: &Jp2kDecodeJob<'_>) -> Result<CpuTile, WsiError> {
    decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Auto)
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
