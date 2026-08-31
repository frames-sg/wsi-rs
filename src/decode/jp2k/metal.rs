use j2k_core::{BackendRequest as J2kBackendRequest, PixelFormat as J2kPixelFormat};
use j2k_metal::{J2kDecoder as J2kMetalJp2kDecoder, MetalDecodeRequest};

use super::prepare::PreparedJp2kJob;
use super::Jp2kColorSpace;
use crate::error::WsiError;

pub(super) fn decode_prepared_jp2k_metal(
    job: &PreparedJp2kJob<'_>,
    sessions: &crate::output::metal::MetalBackendSessions,
) -> Result<crate::output::metal::MetalDeviceTile, WsiError> {
    let mut decoder =
        J2kMetalJp2kDecoder::new(job.input).map_err(|err| WsiError::Jp2k(err.to_string()))?;
    let surface = decoder
        .decode_request_to_device_with_session(
            MetalDecodeRequest::full(J2kPixelFormat::Rgb8, J2kBackendRequest::Metal),
            sessions.j2k(),
        )
        .map_err(|err| WsiError::Jp2k(format!("strict JP2K Metal decode failed: {err}")))?;
    metal_tile_from_jp2k_surface(
        surface,
        job.expected_width,
        job.expected_height,
        job.output_colorspace,
        sessions,
    )
}

fn metal_tile_from_jp2k_surface(
    surface: j2k_metal::Surface,
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
    sessions: &crate::output::metal::MetalBackendSessions,
) -> Result<crate::output::metal::MetalDeviceTile, WsiError> {
    let tile = resident_metal_jp2k_tile(surface)?;
    if colorspace == Jp2kColorSpace::YCbCr {
        let converter = sessions.ycbcr_to_rgb8_converter()?;
        return tile
            .ycbcr8_to_rgb8(&converter)?
            .crop_top_left(expected_width, expected_height);
    }
    tile.crop_top_left(expected_width, expected_height)
}

fn resident_metal_jp2k_tile(
    surface: j2k_metal::Surface,
) -> Result<crate::output::metal::MetalDeviceTile, WsiError> {
    crate::output::metal::MetalDeviceTile::from_j2k(surface)?.ok_or_else(|| WsiError::Unsupported {
        reason: "strict JP2K Metal decode returned a non-resident surface".into(),
    })
}
