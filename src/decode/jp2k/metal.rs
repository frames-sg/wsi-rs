use j2k::DecodeRequest;
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
    crate::core::execution_telemetry::record(
        crate::core::execution_telemetry::Event::MetalSingleDecodes,
        1,
    );
    let surface = decoder
        .decode_request_to_device_with_session(metal_request(job.request)?, sessions.j2k())
        .map_err(|err| WsiError::Jp2k(format!("strict JP2K Metal decode failed: {err}")))?;
    #[cfg(feature = "route-telemetry")]
    crate::core::execution_telemetry::record_metal_pools(sessions);
    metal_tile_from_jp2k_surface(
        surface,
        job.expected_width,
        job.expected_height,
        job.output_colorspace,
        sessions,
    )
    .and_then(|tile| sessions.retain_readback_queue(tile))
}

fn metal_request(request: DecodeRequest) -> Result<MetalDecodeRequest, WsiError> {
    let (fmt, backend) = (J2kPixelFormat::Rgb8, J2kBackendRequest::Metal);
    Ok(match request {
        DecodeRequest::Full => MetalDecodeRequest::full(fmt, backend),
        DecodeRequest::Region { roi } => MetalDecodeRequest::region(fmt, roi, backend),
        DecodeRequest::Reduced { scale } => MetalDecodeRequest::scaled(fmt, scale, backend),
        DecodeRequest::RegionReduced { roi, scale } => {
            MetalDecodeRequest::region_scaled(fmt, roi, scale, backend)
        }
        other => {
            return Err(WsiError::Unsupported {
                reason: format!("no JP2K Metal decode for {other:?}"),
            })
        }
    })
}

fn metal_tile_from_jp2k_surface(
    surface: j2k_metal::Surface,
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
    sessions: &crate::output::metal::MetalBackendSessions,
) -> Result<crate::output::metal::MetalDeviceTile, WsiError> {
    let tile = resident_metal_jp2k_tile(surface)?.crop_top_left(expected_width, expected_height)?;
    if colorspace == Jp2kColorSpace::YCbCr {
        let converter = sessions.ycbcr_to_rgb8_converter()?;
        return tile.ycbcr8_to_rgb8(&converter);
    }
    Ok(tile)
}

pub(super) fn resident_metal_jp2k_tile(
    surface: j2k_metal::Surface,
) -> Result<crate::output::metal::MetalDeviceTile, WsiError> {
    crate::output::metal::MetalDeviceTile::from_j2k(surface)?.ok_or_else(|| WsiError::Unsupported {
        reason: "strict JP2K Metal decode returned a non-resident surface".into(),
    })
}
