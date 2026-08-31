mod input;

#[cfg(test)]
mod tests;

use input::{
    checked_jpeg_rgb_len, decode_jpeg_rgb_with_color_transform_and_patch, expand_grayscale_to_rgb,
    j2k_downscale_for_dimensions, prepare_jpeg_input, resize_jpeg_rgb_nearest,
    try_decode_jpeg_rgb_scaled,
};
pub(crate) use input::{decode_jpeg_rgb_with_color_transform, jpeg_dimensions};

use std::borrow::Cow;

use crate::core::types::{
    ColorSpace, Compression, CpuTile, EncodedTilePhotometricInterpretation, RawCompressedTile,
};
use crate::error::WsiError;
use j2k_jpeg::{
    decode_tiles_into_with_options, decode_tiles_scaled_into_with_options,
    ColorSpace as J2kColorSpace, ColorTransform as J2kColorTransform,
    DecodeOptions as J2kDecodeOptions, Decoder as J2kJpegDecoder, Downscale as J2kDownscale,
    JpegView as J2kJpegView, PixelFormat as J2kPixelFormat,
    TileBatchOptions as J2kTileBatchOptions, TileDecodeJob as J2kTileDecodeJob,
    TileScaledDecodeJob as J2kTileScaledDecodeJob,
};
use rayon::prelude::*;

/// Maximum total bytes allowed for a single JPEG decode allocation.
/// Set to 512 MB to cover large NDPI full-decode levels while preventing
/// OOM from crafted JPEG headers with extreme dimensions.
const MAX_JPEG_DECODE_BYTES: u64 = 128 * 1024 * 1024;
const JPEG_MAX_DIMENSION: u16 = 65500;

pub(crate) const fn is_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
    )
}

pub(crate) struct DecodedJpegRgb {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub(crate) fn standalone_raw_jpeg_tile(data: Vec<u8>) -> Result<RawCompressedTile, WsiError> {
    if !data.starts_with(&[0xFF, 0xD8]) || !data.ends_with(&[0xFF, 0xD9]) {
        return Err(WsiError::Unsupported {
            reason: "raw JPEG passthrough requires a standalone SOI/EOI-delimited image".into(),
        });
    }

    let info = J2kJpegDecoder::inspect(&data).map_err(|err| WsiError::Unsupported {
        reason: format!("raw JPEG passthrough could not inspect the image: {err}"),
    })?;
    if info.bit_depth != 8 {
        return Err(WsiError::Unsupported {
            reason: format!(
                "raw JPEG passthrough requires 8-bit samples, got {}-bit",
                info.bit_depth
            ),
        });
    }

    let (samples_per_pixel, photometric_interpretation) = match info.color_space {
        J2kColorSpace::Grayscale if info.sampling.len() == 1 => {
            (1, EncodedTilePhotometricInterpretation::Monochrome2)
        }
        J2kColorSpace::Rgb if info.sampling.len() == 3 => {
            (3, EncodedTilePhotometricInterpretation::Rgb)
        }
        J2kColorSpace::YCbCr if info.sampling.len() == 3 => {
            (3, EncodedTilePhotometricInterpretation::YbrFull422)
        }
        color_space => {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "raw JPEG passthrough supports 8-bit Gray, RGB, or YCbCr images, got {:?} with {} components",
                    color_space,
                    info.sampling.len()
                ),
            });
        }
    };

    Ok(RawCompressedTile::builder(Compression::Jpeg)
        .dimensions(info.dimensions.0, info.dimensions.1)
        .bits_allocated(u16::from(info.bit_depth))
        .samples_per_pixel(samples_per_pixel)
        .photometric_interpretation(photometric_interpretation)
        .data(data)
        .build()?)
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
    pub color_transform: J2kColorTransform,
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
    color_transform: J2kColorTransform,
}

struct PreparedBatchJpeg<'a> {
    input: Cow<'a, [u8]>,
    output_width: u32,
    output_height: u32,
    output_len: usize,
    stride: usize,
    scale: J2kDownscale,
    grayscale: bool,
}

pub(crate) fn decode_jpeg_rgb_with_size_override(
    data: &[u8],
    tables: Option<&[u8]>,
    image_width: u32,
    image_height: u32,
    requested_width: Option<u32>,
    requested_height: Option<u32>,
    color_transform: J2kColorTransform,
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
            match try_decode_jpeg_rgb_scaled(ScaledJpegDecode {
                data,
                tables,
                expected_width: image_width,
                expected_height: image_height,
                requested_width,
                requested_height,
                force_dimensions: true,
                color_transform,
            })? {
                Some(decoded) => Ok(decoded),
                None => {
                    let decoded = decode_jpeg_rgb_with_color_transform_and_patch(
                        data,
                        tables,
                        image_width,
                        image_height,
                        true,
                        color_transform,
                    )?;
                    resize_jpeg_rgb_nearest(decoded, requested_width, requested_height)
                }
            }
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
        if let Some(results) = try_decode_batch_jpeg_with_j2k(jobs) {
            return results;
        }
    }
    if jobs.len() <= 1 {
        return jobs.iter().map(decode_one_jpeg_job).collect();
    }
    jobs.par_iter().map(decode_one_jpeg_job).collect()
}

fn try_decode_batch_jpeg_with_j2k<'a>(
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
        let prepared_job = prepare_j2k_batch_jpeg_job(job)?;
        needs_scaled_api |= prepared_job.scale != J2kDownscale::None;
        prepared.push(prepared_job);
    }

    let grayscale = prepared.first()?.grayscale;
    if prepared.iter().any(|job| job.grayscale != grayscale) {
        return None;
    }
    let pixel_format = if grayscale {
        J2kPixelFormat::Gray8
    } else {
        J2kPixelFormat::Rgb8
    };

    let decode_options = J2kDecodeOptions::default().with_color_transform(color_transform);
    let mut outputs = prepared
        .iter()
        .map(|job| vec![0u8; job.output_len])
        .collect::<Vec<_>>();
    let batch_options = J2kTileBatchOptions::default();

    if needs_scaled_api {
        let mut batch_jobs = prepared
            .iter()
            .zip(outputs.iter_mut())
            .map(|(job, output)| J2kTileScaledDecodeJob {
                input: job.input.as_ref(),
                out: output.as_mut_slice(),
                stride: job.stride,
                scale: job.scale,
            })
            .collect::<Vec<_>>();
        decode_tiles_scaled_into_with_options(
            &mut batch_jobs,
            pixel_format,
            decode_options,
            batch_options,
        )
        .ok()?;
    } else {
        let mut batch_jobs = prepared
            .iter()
            .zip(outputs.iter_mut())
            .map(|(job, output)| J2kTileDecodeJob {
                input: job.input.as_ref(),
                out: output.as_mut_slice(),
                stride: job.stride,
            })
            .collect::<Vec<_>>();
        decode_tiles_into_with_options(
            &mut batch_jobs,
            pixel_format,
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
                let pixels = if job.grayscale {
                    expand_grayscale_to_rgb(pixels)?
                } else {
                    pixels
                };
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

fn prepare_j2k_batch_jpeg_job<'j, 'a>(job: &'j JpegDecodeJob<'a>) -> Option<PreparedBatchJpeg<'j>> {
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
            let scale = j2k_downscale_for_dimensions(
                job.expected_width,
                job.expected_height,
                requested_width,
                requested_height,
            )?;
            (scale, requested_width, requested_height)
        }
        None => (J2kDownscale::None, job.expected_width, job.expected_height),
    };

    let input = prepare_jpeg_input(
        job.data.as_ref(),
        job.tables.as_deref(),
        job.expected_width,
        job.expected_height,
        job.force_dimensions,
    )
    .ok()?;
    let info = J2kJpegView::parse_with_options(
        input.as_ref(),
        J2kDecodeOptions::default().with_color_transform(job.color_transform),
    )
    .ok()?;
    let encoded_dimensions = info.info().dimensions;
    if encoded_dimensions != (job.expected_width, job.expected_height) {
        return None;
    }
    let grayscale = info.info().color_space == J2kColorSpace::Grayscale;
    let rgb_len = checked_jpeg_rgb_len(output_width, output_height).ok()?;
    let output_len = if grayscale { rgb_len / 3 } else { rgb_len };
    let channels = if grayscale { 1 } else { 3 };
    let stride = (output_width as usize).checked_mul(channels)?;

    Some(PreparedBatchJpeg {
        input,
        output_width,
        output_height,
        output_len,
        stride,
        scale,
        grayscale,
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
        match try_decode_jpeg_rgb_scaled(ScaledJpegDecode {
            data: job.data.as_ref(),
            tables: job.tables.as_deref(),
            expected_width: job.expected_width,
            expected_height: job.expected_height,
            requested_width,
            requested_height,
            force_dimensions: false,
            color_transform: job.color_transform,
        })? {
            Some(decoded) => Ok(decoded),
            None => {
                let decoded = decode_jpeg_rgb_with_color_transform(
                    job.data.as_ref(),
                    job.tables.as_deref(),
                    job.expected_width,
                    job.expected_height,
                    job.color_transform,
                )?;
                resize_jpeg_rgb_nearest(decoded, requested_width, requested_height)
            }
        }
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
