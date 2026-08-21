use std::sync::Arc;

use j2k::CpuDecodeParallelism;
use j2k_core::{BackendRequest as J2kBackendRequest, PixelFormat as J2kPixelFormat};
use j2k_metal::{J2kDecoder as J2kMetalJp2kDecoder, MetalDecodeRequest, MetalTileBatch};

use super::cpu::decode_prepared_jp2k_job;
#[cfg(test)]
use super::prepare::prepare_jp2k_job;
use super::prepare::PreparedJp2kJob;
use super::Jp2kColorSpace;
#[cfg(test)]
use super::Jp2kDecodeJob;
use crate::core::types::{DeviceTile, TilePixels};
use crate::error::WsiError;
use crate::output::MetalBackendSessionsRef;

pub(super) fn decode_prepared_jp2k_pixels_metal(
    job: &PreparedJp2kJob<'_>,
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    let Some(metal_sessions) = metal_sessions else {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for j2k without Metal session".into(),
            });
        }
        return decode_prepared_jp2k_job(job, CpuDecodeParallelism::Auto).map(TilePixels::Cpu);
    };
    let mut decoder =
        J2kMetalJp2kDecoder::new(job.input).map_err(|err| WsiError::Jp2k(err.to_string()))?;
    let surface = decoder
        .decode_request_to_device_with_session(
            MetalDecodeRequest::full(J2kPixelFormat::Rgb8, J2kBackendRequest::Metal),
            metal_sessions.j2k(),
        )
        .map_err(|err| WsiError::Jp2k(format!("j2k JP2K device decode failed: {err}")))?;
    tile_pixels_from_jp2k_surface(
        surface,
        job.expected_width,
        job.expected_height,
        job.output_colorspace,
        metal_sessions,
    )
}

#[cfg(test)]
pub(super) fn decode_jp2k_tile_batch_to_pixels(
    reqs: &[Jp2kDecodeJob<'_>],
    require_device: bool,
    metal_sessions: Option<&crate::output::metal::MetalBackendSessions>,
) -> Result<Vec<TilePixels>, WsiError> {
    let prepared = reqs
        .iter()
        .map(prepare_jp2k_job)
        .collect::<Result<Vec<_>, _>>()?;
    decode_prepared_jp2k_tile_batch_to_pixels(&prepared, require_device, metal_sessions)
}

pub(super) fn decode_prepared_jp2k_tile_batch_to_pixels(
    reqs: &[PreparedJp2kJob<'_>],
    require_device: bool,
    metal_sessions: Option<&crate::output::metal::MetalBackendSessions>,
) -> Result<Vec<TilePixels>, WsiError> {
    let Some(metal_sessions) = metal_sessions else {
        return if require_device {
            Err(WsiError::Unsupported {
                reason: "device backend not available for j2k without Metal session".into(),
            })
        } else {
            Err(WsiError::Unsupported {
                reason: "device backend not requested without Metal session".into(),
            })
        };
    };
    if jp2k_device_batch_enabled() {
        if let Ok(tiles) = decode_prepared_jp2k_tile_batch_to_device_pixels(reqs, metal_sessions) {
            return Ok(tiles);
        }
    }
    let surfaces = reqs
        .iter()
        .map(|req| {
            let mut decoder = J2kMetalJp2kDecoder::new(req.input)
                .map_err(|err| WsiError::Jp2k(err.to_string()))?;
            decoder
                .decode_request_to_device_with_session(
                    MetalDecodeRequest::full(J2kPixelFormat::Rgb8, J2kBackendRequest::Metal),
                    metal_sessions.j2k(),
                )
                .map_err(|err| WsiError::Jp2k(format!("j2k JP2K device decode failed: {err}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    surfaces
        .into_iter()
        .zip(reqs.iter())
        .map(|(surface, req)| {
            tile_pixels_from_jp2k_surface(
                surface,
                req.expected_width,
                req.expected_height,
                req.output_colorspace,
                metal_sessions,
            )
        })
        .collect()
}

fn jp2k_device_batch_enabled() -> bool {
    parse_jp2k_device_batch_flag(std::env::var("WSI_RS_JP2K_DEVICE_BATCH").ok().as_deref())
}

pub(super) fn parse_jp2k_device_batch_flag(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

#[cfg(test)]
pub(super) fn decode_jp2k_tile_batch_to_device_pixels(
    reqs: &[Jp2kDecodeJob<'_>],
    metal_sessions: &crate::output::metal::MetalBackendSessions,
) -> Result<Vec<TilePixels>, WsiError> {
    let prepared = reqs
        .iter()
        .map(prepare_jp2k_job)
        .collect::<Result<Vec<_>, _>>()?;
    decode_prepared_jp2k_tile_batch_to_device_pixels(&prepared, metal_sessions)
}

pub(super) fn decode_prepared_jp2k_tile_batch_to_device_pixels(
    reqs: &[PreparedJp2kJob<'_>],
    metal_sessions: &crate::output::metal::MetalBackendSessions,
) -> Result<Vec<TilePixels>, WsiError> {
    let mut batch = MetalTileBatch::with_capacity(reqs.len());
    for req in reqs {
        batch
            .push_shared_tile_request(
                Arc::<[u8]>::from(req.input),
                MetalDecodeRequest::full(J2kPixelFormat::Rgb8, J2kBackendRequest::Metal),
            )
            .map_err(|err| WsiError::Jp2k(format!("j2k JP2K device batch submit failed: {err}")))?;
    }
    let surfaces = batch
        .decode_all()
        .map_err(|err| WsiError::Jp2k(format!("j2k JP2K device batch decode failed: {err}")))?;
    let mut pixels = Vec::with_capacity(surfaces.len());
    let mut ycbcr_slots = Vec::new();
    let mut ycbcr_tiles = Vec::new();
    for (surface, req) in surfaces.into_iter().zip(reqs.iter()) {
        if req.output_colorspace == Jp2kColorSpace::YCbCr {
            let tile = resident_metal_jp2k_tile(surface)?
                .crop_top_left(req.expected_width, req.expected_height)?;
            ycbcr_slots.push(pixels.len());
            ycbcr_tiles.push(tile);
            pixels.push(None);
            continue;
        }

        pixels.push(Some(tile_pixels_from_jp2k_surface(
            surface,
            req.expected_width,
            req.expected_height,
            req.output_colorspace,
            metal_sessions,
        )?));
    }
    if !ycbcr_tiles.is_empty() {
        let converted = metal_sessions.ycbcr8_tiles_to_rgb8(&ycbcr_tiles)?;
        if converted.len() != ycbcr_slots.len() {
            return Err(WsiError::Jp2k(
                "Metal JP2K YCbCr batch conversion output count mismatch".into(),
            ));
        }
        for (slot, tile) in ycbcr_slots.into_iter().zip(converted) {
            pixels[slot] = Some(TilePixels::Device(DeviceTile::Metal(tile)));
        }
    }
    pixels
        .into_iter()
        .map(|pixel| {
            pixel.ok_or_else(|| {
                WsiError::Jp2k("Metal JP2K YCbCr batch conversion missing output".into())
            })
        })
        .collect()
}

fn tile_pixels_from_jp2k_surface(
    surface: j2k_metal::Surface,
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
    metal_sessions: &crate::output::metal::MetalBackendSessions,
) -> Result<TilePixels, WsiError> {
    let tile = resident_metal_jp2k_tile(surface)?;
    if colorspace == Jp2kColorSpace::YCbCr {
        let converter = metal_sessions.ycbcr_to_rgb8_converter()?;
        return tile
            .ycbcr8_to_rgb8(&converter)
            .and_then(|tile| tile.crop_top_left(expected_width, expected_height))
            .map(|tile| TilePixels::Device(DeviceTile::Metal(tile)));
    }
    let tile = tile.crop_top_left(expected_width, expected_height)?;
    Ok(TilePixels::Device(DeviceTile::Metal(tile)))
}

fn resident_metal_jp2k_tile(
    surface: j2k_metal::Surface,
) -> Result<crate::output::metal::MetalDeviceTile, WsiError> {
    let Some(tile) = crate::output::metal::MetalDeviceTile::from_j2k(surface)? else {
        return Err(WsiError::Jp2k(
            "explicit Metal JP2K decode returned a non-resident surface".into(),
        ));
    };
    Ok(tile)
}
