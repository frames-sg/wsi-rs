use crate::core::decode_runtime::{current_decode_runtime, DecodeRuntime};
#[cfg(all(any(feature = "metal", feature = "cuda"), test))]
use crate::core::types::PixelFormat;
use crate::core::types::{ColorSpace, CpuTile, CpuTileData, CpuTileLayout};
#[cfg(any(feature = "metal", feature = "cuda"))]
use crate::core::types::{DeviceTile, TilePixels};
use crate::decode::jp2k_backend::{effective_output_colorspace, DecodedInterleavedImage};
use crate::decode::jp2k_codestream::{parse_codestream_header, validate_narrow_subset};
#[cfg(debug_assertions)]
use crate::decode::jp2k_packet::parse_tile_part_packets;
use crate::decode::jp2k_raster::{crop_sample_buffer, interleaved_image_to_sample_buffer};
use crate::error::WsiError;
#[cfg(test)]
use image::RgbaImage;
use std::borrow::Cow;

use j2k::{
    decode_tiles_into as j2k_decode_jp2k_tiles_into, CpuDecodeParallelism,
    J2kDecoder as J2kJp2kDecoder, TileBatchOptions as J2kTileBatchOptions,
    TileDecodeJob as J2kJp2kTileDecodeJob,
};
#[cfg(any(feature = "metal", feature = "cuda"))]
use j2k_core::DeviceSurface as J2kDeviceSurface;
use j2k_core::{BackendRequest as J2kBackendRequest, PixelFormat as J2kPixelFormat};
#[cfg(feature = "metal")]
use j2k_metal::SurfaceResidency as J2kJp2kSurfaceResidency;
#[cfg(feature = "metal")]
use j2k_metal::{J2kDecoder as J2kMetalJp2kDecoder, MetalDecodeRequest, MetalTileBatch};
#[cfg(feature = "metal")]
use std::sync::Arc;

#[cfg(feature = "metal")]
type MetalBackendSessionsRef<'a> = Option<&'a crate::output::metal::MetalBackendSessions>;
#[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "metal")))]
type MetalBackendSessionsRef<'a> = Option<&'a ()>;
#[cfg(feature = "cuda")]
type CudaBackendSessionsRef<'a> = Option<&'a crate::output::cuda::CudaBackendSessions>;
#[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "cuda")))]
type CudaBackendSessionsRef<'a> = Option<&'a ()>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jp2kColorSpace {
    Rgb,
    YCbCr,
}

#[derive(Debug, Clone)]
pub(crate) struct Jp2kDecodeJob<'a> {
    pub data: Cow<'a, [u8]>,
    pub expected_width: u32,
    pub expected_height: u32,
    pub rgb_color_space: bool,
    pub backend: J2kBackendRequest,
}

#[cfg(test)]
#[inline]
pub(crate) fn dimensions_from_bounds(x0: u32, x1: u32, y0: u32, y1: u32) -> Option<(u32, u32)> {
    Some((x1.checked_sub(x0)?, y1.checked_sub(y0)?))
}

/// Decode a raw JPEG2000 codestream (J2K, not JP2 container) into a
/// premultiplied RGBA image with alpha = 255.
#[cfg(test)]
pub fn decode_jp2k(
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
    let header = validate_jp2k_decode_request(data, expected_width, expected_height)?;
    let output_colorspace = effective_output_colorspace(&header, colorspace);
    match backend {
        J2kBackendRequest::Auto | J2kBackendRequest::Cpu => decode_jp2k_to_sample_buffer_cpu(
            data,
            expected_width,
            expected_height,
            output_colorspace,
            parallelism,
        ),
        J2kBackendRequest::Metal | J2kBackendRequest::Cuda => Err(WsiError::Unsupported {
            reason: "device backend not available for CPU JP2K sample-buffer decode".into(),
        }),
    }
}

#[cfg(test)]
pub(crate) fn decode_jp2k_tile_batch_to_sample_buffers(
    reqs: &[Jp2kDecodeJob<'_>],
) -> Result<Vec<CpuTile>, WsiError> {
    if reqs.is_empty() {
        return Ok(Vec::new());
    }
    decode_jp2k_tile_batch_with_j2k(reqs)
}

pub(crate) fn decode_batch_jp2k(jobs: &[Jp2kDecodeJob<'_>]) -> Vec<Result<CpuTile, WsiError>> {
    let runtime = current_decode_runtime().unwrap_or_else(DecodeRuntime::default_arc);
    decode_batch_jp2k_with_runtime(jobs, &runtime)
}

fn decode_batch_jp2k_with_runtime(
    jobs: &[Jp2kDecodeJob<'_>],
    runtime: &DecodeRuntime,
) -> Vec<Result<CpuTile, WsiError>> {
    if jobs.is_empty() {
        return Vec::new();
    }
    if jobs.len() == 1 {
        return jobs
            .iter()
            .map(|job| decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Auto))
            .collect();
    }
    if let Some(decoded) = try_decode_batch_jp2k_with_j2k(jobs, runtime) {
        return decoded.into_iter().map(Ok).collect();
    }
    if runtime.has_jp2k_cpu_pool() {
        runtime.install_jp2k_cpu(|| {
            use rayon::prelude::*;
            jobs.par_iter()
                .map(|job| decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Serial))
                .collect()
        })
    } else {
        jobs.iter()
            .map(|job| decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Serial))
            .collect()
    }
}

struct PreparedJp2kBatchJob {
    decoded_width: u32,
    decoded_height: u32,
    expected_width: u32,
    expected_height: u32,
    output_colorspace: Jp2kColorSpace,
    row_bytes: usize,
    output_len: usize,
}

fn try_decode_batch_jp2k_with_j2k(
    jobs: &[Jp2kDecodeJob<'_>],
    runtime: &DecodeRuntime,
) -> Option<Vec<CpuTile>> {
    if jobs.len() <= 1 {
        return None;
    }

    let mut prepared = Vec::with_capacity(jobs.len());
    for job in jobs {
        if !matches!(
            job.backend,
            J2kBackendRequest::Auto | J2kBackendRequest::Cpu
        ) {
            return None;
        }
        let header = validate_jp2k_decode_request(
            job.data.as_ref(),
            job.expected_width,
            job.expected_height,
        )
        .ok()?;
        let row_bytes =
            (header.image_width as usize).checked_mul(J2kPixelFormat::Rgb8.bytes_per_pixel())?;
        let output_len = row_bytes.checked_mul(header.image_height as usize)?;
        prepared.push(PreparedJp2kBatchJob {
            decoded_width: header.image_width,
            decoded_height: header.image_height,
            expected_width: job.expected_width,
            expected_height: job.expected_height,
            output_colorspace: effective_output_colorspace(
                &header,
                if job.rgb_color_space {
                    Jp2kColorSpace::Rgb
                } else {
                    Jp2kColorSpace::YCbCr
                },
            ),
            row_bytes,
            output_len,
        });
    }

    let mut outputs = prepared
        .iter()
        .map(|job| vec![0u8; job.output_len])
        .collect::<Vec<_>>();
    let mut batch_jobs = jobs
        .iter()
        .zip(prepared.iter())
        .zip(outputs.iter_mut())
        .map(|((job, prepared), output)| J2kJp2kTileDecodeJob {
            input: job.data.as_ref(),
            out: output.as_mut_slice(),
            stride: prepared.row_bytes,
        })
        .collect::<Vec<_>>();

    j2k_decode_jp2k_tiles_into(
        &mut batch_jobs,
        J2kPixelFormat::Rgb8,
        J2kTileBatchOptions {
            workers: runtime.options().jp2k_cpu_threads(),
        },
    )
    .ok()?;
    drop(batch_jobs);

    materialize_jp2k_batch_outputs(prepared, outputs, runtime).ok()
}

fn materialize_jp2k_batch_outputs(
    prepared: Vec<PreparedJp2kBatchJob>,
    outputs: Vec<Vec<u8>>,
    runtime: &DecodeRuntime,
) -> Result<Vec<CpuTile>, WsiError> {
    if runtime.has_jp2k_cpu_pool() {
        runtime.install_jp2k_cpu(|| {
            use rayon::prelude::*;

            prepared
                .into_par_iter()
                .zip(outputs.into_par_iter())
                .map(|(job, pixels)| {
                    sample_buffer_from_rgb8_bytes(
                        pixels,
                        job.decoded_width,
                        job.decoded_height,
                        job.expected_width,
                        job.expected_height,
                        job.output_colorspace,
                    )
                })
                .collect()
        })
    } else {
        prepared
            .into_iter()
            .zip(outputs)
            .map(|(job, pixels)| {
                sample_buffer_from_rgb8_bytes(
                    pixels,
                    job.decoded_width,
                    job.decoded_height,
                    job.expected_width,
                    job.expected_height,
                    job.output_colorspace,
                )
            })
            .collect()
    }
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(crate) fn decode_batch_jp2k_pixels(
    jobs: &[Jp2kDecodeJob<'_>],
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Vec<Result<TilePixels, WsiError>> {
    if jobs.is_empty() {
        return Vec::new();
    }
    #[cfg(feature = "cuda")]
    if cuda_sessions.is_some()
        || jobs
            .iter()
            .any(|job| matches!(job.backend, J2kBackendRequest::Cuda))
    {
        return jobs
            .iter()
            .map(|job| decode_one_jp2k_pixels(job, require_device, metal_sessions, cuda_sessions))
            .collect();
    }
    #[cfg(feature = "metal")]
    match decode_jp2k_tile_batch_to_pixels(jobs, require_device, metal_sessions) {
        Ok(tiles) => tiles.into_iter().map(Ok).collect(),
        Err(_) => jobs
            .iter()
            .map(|job| decode_one_jp2k_pixels(job, require_device, metal_sessions, cuda_sessions))
            .collect(),
    }
    #[cfg(not(feature = "metal"))]
    jobs.iter()
        .map(|job| decode_one_jp2k_pixels(job, require_device, metal_sessions, cuda_sessions))
        .collect()
}

#[cfg(test)]
fn decode_one_jp2k_job(job: &Jp2kDecodeJob<'_>) -> Result<CpuTile, WsiError> {
    decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Auto)
}

fn decode_one_jp2k_job_with_parallelism(
    job: &Jp2kDecodeJob<'_>,
    parallelism: CpuDecodeParallelism,
) -> Result<CpuTile, WsiError> {
    let colorspace = if job.rgb_color_space {
        Jp2kColorSpace::Rgb
    } else {
        Jp2kColorSpace::YCbCr
    };
    decode_jp2k_to_sample_buffer_with_backend_and_parallelism(
        job.data.as_ref(),
        job.expected_width,
        job.expected_height,
        colorspace,
        job.backend,
        parallelism,
    )
    .map_err(|err| WsiError::Codec {
        codec: "j2k",
        source: Box::new(err),
    })
}

#[cfg(any(feature = "metal", feature = "cuda"))]
fn decode_one_jp2k_pixels(
    job: &Jp2kDecodeJob<'_>,
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    #[cfg(not(feature = "metal"))]
    let _ = metal_sessions;
    #[cfg(not(feature = "cuda"))]
    let _ = cuda_sessions;
    #[cfg(feature = "cuda")]
    if cuda_sessions.is_some() || matches!(job.backend, J2kBackendRequest::Cuda) {
        return decode_one_jp2k_pixels_cuda(job, require_device, cuda_sessions);
    }

    #[cfg(feature = "metal")]
    {
        return decode_one_jp2k_pixels_metal(job, require_device, metal_sessions);
    }

    #[allow(unreachable_code)]
    if require_device {
        Err(WsiError::Unsupported {
            reason: "device backend not available for j2k".into(),
        })
    } else {
        decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Auto).map(TilePixels::Cpu)
    }
}

#[cfg(feature = "metal")]
fn decode_one_jp2k_pixels_metal(
    job: &Jp2kDecodeJob<'_>,
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    let Some(metal_sessions) = metal_sessions else {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for j2k without Metal session".into(),
            });
        }
        return decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Auto)
            .map(TilePixels::Cpu);
    };
    let header =
        validate_jp2k_decode_request(job.data.as_ref(), job.expected_width, job.expected_height)?;
    let colorspace = effective_output_colorspace(
        &header,
        if job.rgb_color_space {
            Jp2kColorSpace::Rgb
        } else {
            Jp2kColorSpace::YCbCr
        },
    );
    let mut decoder = J2kMetalJp2kDecoder::new(job.data.as_ref())
        .map_err(|err| WsiError::Jp2k(err.to_string()))?;
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
        colorspace,
        require_device,
        Some(metal_sessions),
    )
}

#[cfg(feature = "cuda")]
fn decode_one_jp2k_pixels_cuda(
    job: &Jp2kDecodeJob<'_>,
    require_device: bool,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    let Some(cuda_sessions) = cuda_sessions else {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for j2k without CUDA session".into(),
            });
        }
        return decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Auto)
            .map(TilePixels::Cpu);
    };
    let header =
        validate_jp2k_decode_request(job.data.as_ref(), job.expected_width, job.expected_height)?;
    let colorspace = effective_output_colorspace(
        &header,
        if job.rgb_color_space {
            Jp2kColorSpace::Rgb
        } else {
            Jp2kColorSpace::YCbCr
        },
    );
    let surface = cuda_sessions.with_j2k(|session| {
        let mut decoder = j2k_cuda::J2kDecoder::new(job.data.as_ref())
            .map_err(|err| WsiError::Jp2k(err.to_string()))?;
        decoder
            .decode_to_device_with_session(J2kPixelFormat::Rgb8, session)
            .map_err(cuda_jp2k_decode_error)
    });

    match surface {
        Ok(surface) => match tile_pixels_from_cuda_jp2k_surface(
            surface,
            job.expected_width,
            job.expected_height,
            colorspace,
            require_device,
        ) {
            Ok(tile) => Ok(tile),
            Err(err) if require_device => Err(err),
            Err(_) => decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Auto)
                .map(TilePixels::Cpu),
        },
        Err(err) if require_device => Err(err),
        Err(_) => decode_one_jp2k_job_with_parallelism(job, CpuDecodeParallelism::Auto)
            .map(TilePixels::Cpu),
    }
}

#[cfg(feature = "cuda")]
fn tile_pixels_from_cuda_jp2k_surface(
    surface: j2k_cuda::Surface,
    job_expected_width: u32,
    job_expected_height: u32,
    colorspace: Jp2kColorSpace,
    require_device: bool,
) -> Result<TilePixels, WsiError> {
    if surface.backend_kind() != j2k_core::BackendKind::Cuda {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for j2k".into(),
            });
        }
        let _ = (job_expected_width, job_expected_height, colorspace);
        return Err(WsiError::Unsupported {
            reason: "JP2K CUDA decode returned host surface".into(),
        });
    }
    if surface.residency() == j2k_cuda::SurfaceResidency::CpuStagedCudaUpload {
        if require_device {
            return Err(WsiError::Unsupported {
                reason:
                    "JP2K device decode produced CPU-staged CUDA upload instead of resident CUDA decode"
                        .into(),
            });
        }
        return Err(WsiError::Unsupported {
            reason: "JP2K CUDA decode produced CPU-staged CUDA upload".into(),
        });
    }
    if surface.residency() != j2k_cuda::SurfaceResidency::CudaResidentDecode
        || surface.cuda_surface().is_none()
    {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "JP2K CUDA decode did not return a resident CUDA surface".into(),
            });
        }
        let _ = (job_expected_width, job_expected_height, colorspace);
        return Err(WsiError::Unsupported {
            reason: "JP2K CUDA decode did not return a resident CUDA surface".into(),
        });
    }
    if colorspace == Jp2kColorSpace::YCbCr {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "JP2K CUDA YCbCr output requires resident CUDA RGB conversion, which wsi-rs does not own".into(),
            });
        }
        return Err(WsiError::Unsupported {
            reason: "JP2K CUDA YCbCr output requires CUDA RGB conversion".into(),
        });
    }
    if let Some(tile) = crate::output::cuda::CudaDeviceTile::from_j2k(surface)? {
        return Ok(TilePixels::Device(DeviceTile::Cuda(tile)));
    }
    if require_device {
        return Err(WsiError::Unsupported {
            reason: "device backend not available for j2k".into(),
        });
    }
    Err(WsiError::Unsupported {
        reason: "JP2K CUDA decode did not produce a public CUDA surface".into(),
    })
}

#[cfg(feature = "cuda")]
fn cuda_jp2k_decode_error(err: j2k_cuda::Error) -> WsiError {
    WsiError::Unsupported {
        reason: format!("JP2K CUDA device decode failed: {err}"),
    }
}

fn validate_jp2k_decode_request(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
) -> Result<crate::decode::jp2k_codestream::Jp2kCodestreamInfo, WsiError> {
    if data.is_empty() {
        return Err(WsiError::Jp2k("empty JP2K data".into()));
    }

    let header = parse_codestream_header(data)?;
    validate_narrow_subset(&header)?;
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

    #[cfg(debug_assertions)]
    if let Some(tile_part) = header.tile_parts.first() {
        let _ = parse_tile_part_packets(data, &header, tile_part);
    }

    Ok(header)
}

fn decode_jp2k_to_sample_buffer_cpu(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
    parallelism: CpuDecodeParallelism,
) -> Result<CpuTile, WsiError> {
    let mut decoder = J2kJp2kDecoder::new(data).map_err(|err| WsiError::Jp2k(err.to_string()))?;
    decoder.set_cpu_decode_parallelism(parallelism);
    let (width, height) = decoder.info().dimensions;
    let row_bytes = (width as usize)
        .checked_mul(J2kPixelFormat::Rgb8.bytes_per_pixel())
        .ok_or_else(|| WsiError::Jp2k("j2k JP2K row byte count overflow".into()))?;
    let len = row_bytes
        .checked_mul(height as usize)
        .ok_or_else(|| WsiError::Jp2k("j2k JP2K output size overflow".into()))?;
    let mut rgb = vec![0; len];

    decoder
        .decode_into(&mut rgb, row_bytes, J2kPixelFormat::Rgb8)
        .map_err(|err| WsiError::Jp2k(format!("j2k JP2K decode failed: {err}")))?;

    sample_buffer_from_rgb8_bytes(
        rgb,
        width,
        height,
        expected_width,
        expected_height,
        colorspace,
    )
}

#[cfg(test)]
fn decode_jp2k_tile_batch_with_j2k(reqs: &[Jp2kDecodeJob<'_>]) -> Result<Vec<CpuTile>, WsiError> {
    reqs.iter().map(decode_one_jp2k_job).collect()
}

#[cfg(feature = "metal")]
fn decode_jp2k_tile_batch_to_pixels(
    reqs: &[Jp2kDecodeJob<'_>],
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
        if let Ok(tiles) =
            decode_jp2k_tile_batch_to_device_pixels(reqs, require_device, metal_sessions)
        {
            return Ok(tiles);
        }
    }
    let headers = reqs
        .iter()
        .map(|req| {
            validate_jp2k_decode_request(req.data.as_ref(), req.expected_width, req.expected_height)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let surfaces = reqs
        .iter()
        .map(|req| {
            let mut decoder = J2kMetalJp2kDecoder::new(req.data.as_ref())
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
        .zip(reqs.iter().zip(headers.iter()))
        .map(|(surface, (req, header))| {
            tile_pixels_from_jp2k_surface(
                surface,
                req.expected_width,
                req.expected_height,
                effective_output_colorspace(
                    header,
                    if req.rgb_color_space {
                        Jp2kColorSpace::Rgb
                    } else {
                        Jp2kColorSpace::YCbCr
                    },
                ),
                require_device,
                Some(metal_sessions),
            )
        })
        .collect()
}

#[cfg(feature = "metal")]
fn jp2k_device_batch_enabled() -> bool {
    parse_jp2k_device_batch_flag(std::env::var("WSI_RS_JP2K_DEVICE_BATCH").ok().as_deref())
}

#[cfg(feature = "metal")]
fn parse_jp2k_device_batch_flag(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

#[cfg(feature = "metal")]
fn decode_jp2k_tile_batch_to_device_pixels(
    reqs: &[Jp2kDecodeJob<'_>],
    require_device: bool,
    metal_sessions: &crate::output::metal::MetalBackendSessions,
) -> Result<Vec<TilePixels>, WsiError> {
    let headers = reqs
        .iter()
        .map(|req| {
            validate_jp2k_decode_request(req.data.as_ref(), req.expected_width, req.expected_height)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let output_colorspaces = reqs
        .iter()
        .zip(headers.iter())
        .map(|(req, header)| {
            effective_output_colorspace(
                header,
                if req.rgb_color_space {
                    Jp2kColorSpace::Rgb
                } else {
                    Jp2kColorSpace::YCbCr
                },
            )
        })
        .collect::<Vec<_>>();
    let conversion_sessions = output_colorspaces
        .contains(&Jp2kColorSpace::YCbCr)
        .then_some(metal_sessions);
    let mut batch = MetalTileBatch::with_capacity(reqs.len());
    for req in reqs {
        batch
            .push_shared_tile_request(
                Arc::<[u8]>::from(req.data.as_ref()),
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
    for (surface, ((req, _header), colorspace)) in surfaces.into_iter().zip(
        reqs.iter()
            .zip(headers.iter())
            .zip(output_colorspaces.iter()),
    ) {
        if *colorspace == Jp2kColorSpace::YCbCr
            && surface.backend_kind() == j2k_core::BackendKind::Metal
        {
            if surface.residency() == J2kJp2kSurfaceResidency::CpuStagedMetalUpload {
                return Err(WsiError::Unsupported {
                    reason:
                        "JP2K device decode produced CPU-staged Metal upload instead of resident Metal decode"
                            .into(),
                });
            }
            if let Some(tile) = crate::output::metal::MetalDeviceTile::from_j2k(surface)? {
                let tile = tile.crop_top_left(req.expected_width, req.expected_height)?;
                ycbcr_slots.push(pixels.len());
                ycbcr_tiles.push(tile);
                pixels.push(None);
                continue;
            }
            if require_device {
                return Err(WsiError::Unsupported {
                    reason: "device backend not available for j2k".into(),
                });
            }
            return Err(WsiError::Jp2k(
                "j2k JP2K returned Metal backend without public buffer".into(),
            ));
        }

        pixels.push(Some(tile_pixels_from_jp2k_surface(
            surface,
            req.expected_width,
            req.expected_height,
            *colorspace,
            require_device,
            conversion_sessions,
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

#[cfg(feature = "metal")]
fn tile_pixels_from_jp2k_surface(
    surface: j2k_metal::Surface,
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
    require_device: bool,
    metal_sessions: Option<&crate::output::metal::MetalBackendSessions>,
) -> Result<TilePixels, WsiError> {
    if surface.backend_kind() == j2k_core::BackendKind::Metal {
        if surface.residency() == J2kJp2kSurfaceResidency::CpuStagedMetalUpload {
            return Err(WsiError::Unsupported {
                reason:
                    "JP2K device decode produced CPU-staged Metal upload instead of resident Metal decode"
                        .into(),
            });
        }
        if let Some(tile) = crate::output::metal::MetalDeviceTile::from_j2k(surface)? {
            if colorspace == Jp2kColorSpace::YCbCr {
                let Some(metal_sessions) = metal_sessions else {
                    return Err(WsiError::Unsupported {
                        reason:
                            "JP2K Metal YCbCr output requires a Metal session for RGB conversion"
                                .into(),
                    });
                };
                let converter = metal_sessions.ycbcr_to_rgb8_converter()?;
                return tile
                    .ycbcr8_to_rgb8(&converter)
                    .and_then(|tile| tile.crop_top_left(expected_width, expected_height))
                    .map(|tile| TilePixels::Device(DeviceTile::Metal(tile)));
            }
            let tile = tile.crop_top_left(expected_width, expected_height)?;
            return Ok(TilePixels::Device(DeviceTile::Metal(tile)));
        }
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for j2k".into(),
            });
        }
        return Err(WsiError::Jp2k(
            "j2k JP2K returned Metal backend without public buffer".into(),
        ));
    }
    if require_device {
        return Err(WsiError::Unsupported {
            reason: "device backend not available for j2k".into(),
        });
    }
    sample_buffer_from_j2k_surface(surface, expected_width, expected_height, colorspace)
        .map(TilePixels::Cpu)
}

fn sample_buffer_from_rgb8_bytes(
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
) -> Result<CpuTile, WsiError> {
    if colorspace == Jp2kColorSpace::Rgb && width == expected_width && height == expected_height {
        let expected_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(|| WsiError::Jp2k("decoded JP2K image size overflow".into()))?;
        if bytes.len() != expected_len {
            return Err(WsiError::Jp2k(format!(
                "unexpected decoded JP2K buffer length: expected {}, found {}",
                expected_len,
                bytes.len()
            )));
        }
        return Ok(CpuTile {
            width,
            height,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(bytes),
        });
    }
    crop_sample_buffer(
        interleaved_image_to_sample_buffer(DecodedInterleavedImage {
            width: width as usize,
            height: height as usize,
            colorspace,
            pixels: bytes,
        })?,
        expected_width,
        expected_height,
    )
}

#[cfg(feature = "metal")]
fn sample_buffer_from_j2k_surface(
    surface: j2k_metal::Surface,
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
) -> Result<CpuTile, WsiError> {
    if surface.pixel_format() != J2kPixelFormat::Rgb8 {
        return Err(WsiError::Jp2k(format!(
            "j2k JP2K returned unsupported pixel format {:?}",
            surface.pixel_format()
        )));
    }
    let (width, height) = surface.dimensions();
    let expected_len = width as usize * height as usize * 3;
    let bytes = surface
        .as_bytes()
        .map_err(|err| WsiError::Jp2k(format!("j2k JP2K surface readback failed: {err}")))?;
    if bytes.len() != expected_len {
        return Err(WsiError::Jp2k(format!(
            "j2k JP2K returned {} bytes for {}x{} RGB8 surface",
            bytes.len(),
            width,
            height
        )));
    }

    sample_buffer_from_rgb8_bytes(
        bytes.to_vec(),
        width,
        height,
        expected_width,
        expected_height,
        colorspace,
    )
}

#[cfg(test)]
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
    for (src, dst) in rgb.chunks_exact(3).zip(rgba.chunks_exact_mut(4)) {
        dst[0] = src[0];
        dst[1] = src[1];
        dst[2] = src[2];
    }
    RgbaImage::from_raw(buffer.width, buffer.height, rgba)
        .ok_or_else(|| WsiError::Jp2k("failed to create RgbaImage from decoded JP2K data".into()))
}

#[cfg(test)]
#[path = "jp2k/tests.rs"]
mod tests;
