use j2k_core::{BackendRequest, PixelFormat};

use crate::decode::jp2k::{Jp2kColorSpace, Jp2kDecodeJob};
use crate::decode::jp2k_backend::effective_output_colorspace;
use crate::decode::jp2k_codestream::{
    codestream_header_from_view, validate_pixel_contract, Jp2kCodestreamInfo,
};
use crate::error::WsiError;

#[derive(Debug, Clone, Copy)]
pub(super) struct PreparedJp2kJob<'a> {
    pub(super) input: &'a [u8],
    pub(super) decoded_width: u32,
    pub(super) decoded_height: u32,
    pub(super) expected_width: u32,
    pub(super) expected_height: u32,
    pub(super) output_colorspace: Jp2kColorSpace,
    pub(super) row_bytes: usize,
    pub(super) output_len: usize,
    pub(super) backend: BackendRequest,
}

pub(super) fn prepare_jp2k_job<'a>(
    job: &'a Jp2kDecodeJob<'_>,
) -> Result<PreparedJp2kJob<'a>, WsiError> {
    prepare_jp2k_input(
        job.data.as_ref(),
        job.expected_width,
        job.expected_height,
        if job.rgb_color_space {
            Jp2kColorSpace::Rgb
        } else {
            Jp2kColorSpace::YCbCr
        },
        job.backend,
    )
}

pub(super) fn prepare_jp2k_input(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
    backend: BackendRequest,
) -> Result<PreparedJp2kJob<'_>, WsiError> {
    prepare_jp2k_input_and_view(data, expected_width, expected_height, colorspace, backend)
        .map(|(job, _)| job)
}

pub(super) fn prepare_jp2k_input_and_view(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
    backend: BackendRequest,
) -> Result<(PreparedJp2kJob<'_>, j2k::J2kView<'_>), WsiError> {
    crate::core::execution_telemetry::record(
        crate::core::execution_telemetry::Event::Jp2kPreparations,
        1,
    );
    if data.is_empty() {
        return Err(WsiError::Jp2k("empty JP2K data".into()));
    }
    let view = j2k::J2kView::parse(data).map_err(|error| WsiError::Jp2k(error.to_string()))?;
    let header = codestream_header_from_view(&view)?;
    validate_jp2k_geometry(&header, expected_width, expected_height)?;
    let row_bytes = (header.image_width as usize)
        .checked_mul(PixelFormat::Rgb8.bytes_per_pixel())
        .ok_or_else(|| WsiError::Jp2k("j2k JP2K row byte count overflow".into()))?;
    let output_len = row_bytes
        .checked_mul(header.image_height as usize)
        .ok_or_else(|| WsiError::Jp2k("j2k JP2K output size overflow".into()))?;
    Ok((
        PreparedJp2kJob {
            input: data,
            decoded_width: header.image_width,
            decoded_height: header.image_height,
            expected_width,
            expected_height,
            output_colorspace: effective_output_colorspace(&header, colorspace),
            row_bytes,
            output_len,
            backend,
        },
        view,
    ))
}

fn validate_jp2k_geometry(
    header: &Jp2kCodestreamInfo,
    expected_width: u32,
    expected_height: u32,
) -> Result<(), WsiError> {
    validate_pixel_contract(header)?;
    if header.image_width < expected_width || header.image_height < expected_height {
        return Err(WsiError::Jp2k(format!(
            "dimension mismatch: expected at least {}x{}, got {}x{}",
            expected_width, expected_height, header.image_width, header.image_height
        )));
    }
    if header.components.len() != 3 {
        return Err(WsiError::Jp2k(format!(
            "expected 3 components, found {}",
            header.components.len()
        )));
    }

    Ok(())
}
