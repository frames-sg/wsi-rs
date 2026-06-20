mod device;
mod input;

#[cfg(test)]
mod tests;

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(crate) use device::decode_batch_jpeg_pixels;
#[cfg(test)]
pub(crate) use input::jpeg_tile_geometry;
use input::{
    checked_jpeg_rgb_len, decode_jpeg_rgb_with_color_transform_and_patch,
    inspect_signinum_jpeg_output_size, prepare_jpeg_input, resize_jpeg_rgb_nearest,
    signinum_downscale_for_dimensions, try_decode_jpeg_rgb_scaled,
};
pub(crate) use input::{decode_jpeg_rgb_with_color_transform, jpeg_dimensions};

use std::borrow::Cow;

use crate::core::types::{ColorSpace, CpuTile};
use crate::error::WsiError;
#[cfg(test)]
use image::RgbaImage;
use rayon::prelude::*;
use signinum_jpeg::{
    decode_tiles_into_with_options, decode_tiles_scaled_into_with_options,
    ColorTransform as SigninumColorTransform, DecodeOptions as SigninumDecodeOptions,
    Downscale as SigninumDownscale, PixelFormat as SigninumPixelFormat,
    TileBatchOptions as SigninumTileBatchOptions, TileDecodeJob as SigninumTileDecodeJob,
    TileScaledDecodeJob as SigninumTileScaledDecodeJob,
};

/// Maximum total bytes allowed for a single JPEG decode allocation.
/// Set to 512 MB to cover large NDPI full-decode levels while preventing
/// OOM from crafted JPEG headers with extreme dimensions.
const MAX_JPEG_DECODE_BYTES: u64 = 512 * 1024 * 1024;
const JPEG_MAX_DIMENSION: u16 = 65500;
pub struct DecodedJpegRgb {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct JpegTileGeometry {
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
}

#[derive(Debug)]
pub(crate) struct JpegDecodeJob<'a> {
    pub data: Cow<'a, [u8]>,
    pub tables: Option<Cow<'a, [u8]>>,
    pub expected_width: u32,
    pub expected_height: u32,
    pub color_transform: SigninumColorTransform,
    pub force_dimensions: bool,
    pub requested_size: Option<(u32, u32)>,
}

struct ScaledJpegDecode<'a> {
    data: &'a [u8],
    tables: Option<&'a [u8]>,
    expected_width: u32,
    expected_height: u32,
    requested_width: u32,
    requested_height: u32,
    force_dimensions: bool,
    color_transform: SigninumColorTransform,
}

struct PreparedBatchJpeg<'a> {
    input: Cow<'a, [u8]>,
    output_width: u32,
    output_height: u32,
    output_len: usize,
    stride: usize,
    scale: SigninumDownscale,
}

/// Decode JPEG data to premultiplied RGBA (alpha=255 for all decoded pixels).
///
/// If `tables` is provided, it is prepended to `data` before decoding.
/// Tables end with FFD9 (EOI), data starts with FFD8 (SOI).
/// Strip EOI from tables, strip SOI from data, concatenate.
#[cfg(test)]
pub fn decode_jpeg(
    data: &[u8],
    tables: Option<&[u8]>,
    expected_width: u32,
    expected_height: u32,
) -> Result<RgbaImage, WsiError> {
    let decoded = decode_jpeg_rgb(data, tables, expected_width, expected_height)?;
    let pixel_count = (decoded.width as usize)
        .checked_mul(decoded.height as usize)
        .ok_or_else(|| WsiError::Jpeg("pixel dimensions overflow".into()))?;
    let rgba_size = pixel_count
        .checked_mul(4)
        .ok_or_else(|| WsiError::Jpeg("RGBA buffer size overflow".into()))?;
    let mut rgba_buf = vec![255u8; rgba_size];
    for (rgb, rgba) in decoded
        .pixels
        .chunks_exact(3)
        .zip(rgba_buf.chunks_exact_mut(4))
    {
        rgba[0] = rgb[0];
        rgba[1] = rgb[1];
        rgba[2] = rgb[2];
    }
    RgbaImage::from_raw(decoded.width, decoded.height, rgba_buf)
        .ok_or_else(|| WsiError::Jpeg("failed to create RgbaImage".into()))
}

#[cfg(test)]
pub fn decode_jpeg_rgb(
    data: &[u8],
    tables: Option<&[u8]>,
    expected_width: u32,
    expected_height: u32,
) -> Result<DecodedJpegRgb, WsiError> {
    decode_jpeg_rgb_with_color_transform(
        data,
        tables,
        expected_width,
        expected_height,
        SigninumColorTransform::Auto,
    )
}

pub(crate) fn decode_jpeg_rgb_with_size_override(
    data: &[u8],
    tables: Option<&[u8]>,
    image_width: u32,
    image_height: u32,
    requested_width: Option<u32>,
    requested_height: Option<u32>,
    color_transform: SigninumColorTransform,
) -> Result<DecodedJpegRgb, WsiError> {
    if image_width == 0
        || image_height == 0
        || image_width > u16::MAX as u32
        || image_height > u16::MAX as u32
    {
        return Err(WsiError::Jpeg(
            "JPEG size override requires nonzero u16 dimensions".into(),
        ));
    }

    match (requested_width, requested_height) {
        (Some(requested_width), Some(requested_height)) => {
            try_decode_jpeg_rgb_scaled(ScaledJpegDecode {
                data,
                tables,
                expected_width: image_width,
                expected_height: image_height,
                requested_width,
                requested_height,
                force_dimensions: true,
                color_transform,
            })?
            .map_or_else(
                || {
                    decode_jpeg_rgb_with_color_transform_and_patch(
                        data,
                        tables,
                        image_width,
                        image_height,
                        true,
                        color_transform,
                    )
                    .and_then(|decoded| {
                        resize_jpeg_rgb_nearest(decoded, requested_width, requested_height)
                    })
                },
                Ok,
            )
        }
        _ => decode_jpeg_rgb_with_color_transform_and_patch(
            data,
            tables,
            image_width,
            image_height,
            true,
            color_transform,
        ),
    }
}

pub(crate) fn decode_batch_jpeg<'a>(jobs: &[JpegDecodeJob<'a>]) -> Vec<Result<CpuTile, WsiError>> {
    if jobs.len() > 1 {
        if let Some(results) = try_decode_batch_jpeg_with_signinum(jobs) {
            return results;
        }
    }
    if jobs.len() <= 1 {
        return jobs.iter().map(decode_one_jpeg_job).collect();
    }
    jobs.par_iter().map(decode_one_jpeg_job).collect()
}

fn try_decode_batch_jpeg_with_signinum<'a>(
    jobs: &[JpegDecodeJob<'a>],
) -> Option<Vec<Result<CpuTile, WsiError>>> {
    let first = jobs.first()?;
    let color_transform = first.color_transform;
    if jobs
        .iter()
        .any(|job| job.color_transform != color_transform)
    {
        return None;
    }

    let mut prepared = Vec::with_capacity(jobs.len());
    let mut needs_scaled_api = false;
    for job in jobs {
        let prepared_job = prepare_signinum_batch_jpeg_job(job)?;
        needs_scaled_api |= prepared_job.scale != SigninumDownscale::None;
        prepared.push(prepared_job);
    }

    let decode_options = SigninumDecodeOptions::default().with_color_transform(color_transform);
    let mut outputs = prepared
        .iter()
        .map(|job| vec![0u8; job.output_len])
        .collect::<Vec<_>>();
    let batch_options = SigninumTileBatchOptions::default();

    if needs_scaled_api {
        let mut batch_jobs = prepared
            .iter()
            .zip(outputs.iter_mut())
            .map(|(job, output)| SigninumTileScaledDecodeJob {
                input: job.input.as_ref(),
                out: output.as_mut_slice(),
                stride: job.stride,
                scale: job.scale,
            })
            .collect::<Vec<_>>();
        decode_tiles_scaled_into_with_options(
            &mut batch_jobs,
            SigninumPixelFormat::Rgb8,
            decode_options,
            batch_options,
        )
        .ok()?;
    } else {
        let mut batch_jobs = prepared
            .iter()
            .zip(outputs.iter_mut())
            .map(|(job, output)| SigninumTileDecodeJob {
                input: job.input.as_ref(),
                out: output.as_mut_slice(),
                stride: job.stride,
            })
            .collect::<Vec<_>>();
        decode_tiles_into_with_options(
            &mut batch_jobs,
            SigninumPixelFormat::Rgb8,
            decode_options,
            batch_options,
        )
        .ok()?;
    }

    Some(
        prepared
            .into_iter()
            .zip(outputs)
            .map(|(job, pixels)| {
                CpuTile::from_u8_interleaved(
                    job.output_width,
                    job.output_height,
                    3,
                    ColorSpace::Rgb,
                    pixels,
                )
            })
            .collect(),
    )
}

fn prepare_signinum_batch_jpeg_job<'j, 'a>(
    job: &'j JpegDecodeJob<'a>,
) -> Option<PreparedBatchJpeg<'j>> {
    if job.expected_width == 0 || job.expected_height == 0 {
        return None;
    }
    if job.force_dimensions
        && (job.expected_width > u16::MAX as u32 || job.expected_height > u16::MAX as u32)
    {
        return None;
    }

    let (scale, output_width, output_height) = match job.requested_size {
        Some((requested_width, requested_height)) => {
            if requested_width == 0 || requested_height == 0 {
                return None;
            }
            let scale = signinum_downscale_for_dimensions(
                job.expected_width,
                job.expected_height,
                requested_width,
                requested_height,
            )?;
            (scale, requested_width, requested_height)
        }
        None => (
            SigninumDownscale::None,
            job.expected_width,
            job.expected_height,
        ),
    };

    let input = prepare_jpeg_input(
        job.data.as_ref(),
        job.tables.as_deref(),
        job.expected_width,
        job.expected_height,
        job.force_dimensions,
    );
    let encoded_dimensions = inspect_signinum_jpeg_output_size(input.as_ref()).ok()?;
    if encoded_dimensions != (job.expected_width, job.expected_height) {
        return None;
    }
    let output_len = checked_jpeg_rgb_len(output_width, output_height).ok()?;
    let stride = (output_width as usize).checked_mul(3)?;

    Some(PreparedBatchJpeg {
        input,
        output_width,
        output_height,
        output_len,
        stride,
        scale,
    })
}

pub(super) fn decode_one_jpeg_job(job: &JpegDecodeJob<'_>) -> Result<CpuTile, WsiError> {
    let decoded = if job.force_dimensions {
        decode_jpeg_rgb_with_size_override(
            job.data.as_ref(),
            job.tables.as_deref(),
            job.expected_width,
            job.expected_height,
            job.requested_size.map(|(width, _)| width),
            job.requested_size.map(|(_, height)| height),
            job.color_transform,
        )
    } else if let Some((requested_width, requested_height)) = job.requested_size {
        try_decode_jpeg_rgb_scaled(ScaledJpegDecode {
            data: job.data.as_ref(),
            tables: job.tables.as_deref(),
            expected_width: job.expected_width,
            expected_height: job.expected_height,
            requested_width,
            requested_height,
            force_dimensions: false,
            color_transform: job.color_transform,
        })?
        .map_or_else(
            || {
                decode_jpeg_rgb_with_color_transform(
                    job.data.as_ref(),
                    job.tables.as_deref(),
                    job.expected_width,
                    job.expected_height,
                    job.color_transform,
                )
                .and_then(|decoded| {
                    resize_jpeg_rgb_nearest(decoded, requested_width, requested_height)
                })
            },
            Ok,
        )
    } else {
        decode_jpeg_rgb_with_color_transform(
            job.data.as_ref(),
            job.tables.as_deref(),
            job.expected_width,
            job.expected_height,
            job.color_transform,
        )
    }
    .map_err(|err| WsiError::Codec {
        codec: "jpeg",
        source: Box::new(err),
    })?;

    CpuTile::from_u8_interleaved(
        decoded.width,
        decoded.height,
        3,
        ColorSpace::Rgb,
        decoded.pixels,
    )
}
