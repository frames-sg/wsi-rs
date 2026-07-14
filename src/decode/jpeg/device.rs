#[cfg(all(feature = "metal", test, target_os = "macos"))]
use std::cell::Cell;

#[cfg(any(feature = "metal", feature = "cuda"))]
use rayon::prelude::*;

#[cfg(feature = "metal")]
use crate::core::types::{ColorSpace, CpuTile};
#[cfg(any(feature = "metal", feature = "cuda"))]
use crate::core::types::{DeviceTile, TilePixels};
#[cfg(any(feature = "metal", feature = "cuda"))]
use crate::error::WsiError;
#[cfg(any(feature = "metal", feature = "cuda"))]
use j2k_core::{
    BackendKind as J2kBackendKind, BackendRequest as J2kBackendRequest, DeviceSurface as _,
};
#[cfg(feature = "cuda")]
use j2k_core::{
    DeviceSubmission as J2kDeviceSubmission, ImageDecode as J2kImageDecode, ImageDecodeSubmit,
};
#[cfg(feature = "metal")]
use j2k_jpeg::ColorTransform as J2kColorTransform;
#[cfg(any(feature = "metal", feature = "cuda"))]
use j2k_jpeg::{
    DecodeOptions as J2kDecodeOptions, JpegView as J2kJpegView, PixelFormat as J2kPixelFormat,
    SofKind as J2kSofKind,
};
#[cfg(feature = "metal")]
use j2k_jpeg_metal::SurfaceResidency as J2kJpegSurfaceResidency;

#[cfg(feature = "metal")]
use super::input::crop_jpeg_rgb_to_expected;
#[cfg(feature = "metal")]
use super::input::inspect_j2k_jpeg_output_size;
#[cfg(any(feature = "metal", feature = "cuda"))]
use super::input::{prepare_jpeg_input, validate_j2k_jpeg_output_size};
#[cfg(feature = "metal")]
use super::DecodedJpegRgb;
#[cfg(any(feature = "metal", feature = "cuda"))]
use super::{decode_one_jpeg_job, JpegDecodeJob};

#[cfg(feature = "metal")]
type MetalBackendSessionsRef<'a> = Option<&'a crate::output::metal::MetalBackendSessions>;
#[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "metal")))]
type MetalBackendSessionsRef<'a> = Option<&'a ()>;
#[cfg(feature = "cuda")]
type CudaBackendSessionsRef<'a> = Option<&'a crate::output::cuda::CudaBackendSessions>;
#[cfg(all(any(feature = "metal", feature = "cuda"), not(feature = "cuda")))]
type CudaBackendSessionsRef<'a> = Option<&'a ()>;
#[cfg(all(feature = "metal", test, target_os = "macos"))]
thread_local! {
    static JPEG_DEVICE_BATCH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(all(feature = "metal", test, target_os = "macos"))]
pub(super) fn reset_jpeg_device_batch_attempts_for_test() {
    JPEG_DEVICE_BATCH_ATTEMPTS.with(|attempts| attempts.set(0));
}

#[cfg(all(feature = "metal", test, target_os = "macos"))]
pub(super) fn jpeg_device_batch_attempts_for_test() -> usize {
    JPEG_DEVICE_BATCH_ATTEMPTS.with(Cell::get)
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(crate) fn decode_batch_jpeg_pixels<'a>(
    jobs: &[JpegDecodeJob<'a>],
    backend: J2kBackendRequest,
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Vec<Result<TilePixels, WsiError>> {
    #[cfg(all(feature = "metal", feature = "cuda"))]
    let route_cuda = cuda_sessions.is_some() || matches!(backend, J2kBackendRequest::Cuda);
    #[cfg(all(feature = "metal", not(feature = "cuda")))]
    let route_cuda = false;

    #[cfg(target_os = "macos")]
    #[cfg(feature = "metal")]
    if !route_cuda {
        if let Some(metal_sessions) = metal_sessions {
            if let Some(decoded) = decode_jpeg_tile_batch_to_device_pixels(
                jobs,
                backend,
                require_device,
                metal_sessions,
            ) {
                return decoded;
            }
        }
    }

    if jobs.len() <= 1 {
        return jobs
            .iter()
            .map(|job| {
                decode_one_jpeg_pixels(job, backend, require_device, metal_sessions, cuda_sessions)
            })
            .collect();
    }
    jobs.par_iter()
        .map(|job| {
            decode_one_jpeg_pixels(job, backend, require_device, metal_sessions, cuda_sessions)
        })
        .collect()
}

#[cfg(all(feature = "metal", target_os = "macos"))]
fn decode_jpeg_tile_batch_to_device_pixels<'a>(
    jobs: &[JpegDecodeJob<'a>],
    backend: J2kBackendRequest,
    require_device: bool,
    metal_sessions: &crate::output::metal::MetalBackendSessions,
) -> Option<Vec<Result<TilePixels, WsiError>>> {
    if jobs.len() < 2
        || metal_sessions.private_jpeg_decode()
        || !matches!(backend, J2kBackendRequest::Auto | J2kBackendRequest::Metal)
    {
        return None;
    }

    let mut prepared = Vec::with_capacity(jobs.len());
    for job in jobs {
        if job.force_dimensions
            || job.requested_size.is_some()
            || !matches!(job.color_transform, J2kColorTransform::Auto)
        {
            return None;
        }

        let input = prepare_jpeg_input(
            job.data.as_ref(),
            job.tables.as_deref(),
            job.expected_width,
            job.expected_height,
            job.force_dimensions,
        );
        let Ok(dimensions) = inspect_j2k_jpeg_output_size(input.as_ref()) else {
            return None;
        };
        if dimensions != (job.expected_width, job.expected_height) {
            return None;
        }
        let Ok(view) = J2kJpegView::parse_with_options(
            input.as_ref(),
            J2kDecodeOptions::default().with_color_transform(job.color_transform),
        ) else {
            return None;
        };
        if view.info().sof_kind == J2kSofKind::Progressive8 {
            return None;
        }
        prepared.push(input);
    }

    #[cfg(all(test, target_os = "macos"))]
    JPEG_DEVICE_BATCH_ATTEMPTS.with(|attempts| attempts.set(attempts.get().saturating_add(1)));

    let inputs = prepared
        .iter()
        .map(|input| input.as_ref())
        .collect::<Vec<_>>();
    let surfaces = match j2k_jpeg_metal::decode_rgb8_batch_to_device_with_session(
        &inputs,
        metal_sessions.jpeg(),
    ) {
        Ok(Some(surfaces)) => surfaces,
        Ok(None) => return None,
        Err(err) if require_device => {
            let reason = format!("JPEG Metal batch decode failed: {err}");
            return Some(
                (0..jobs.len())
                    .map(|_| {
                        Err(WsiError::Unsupported {
                            reason: reason.clone(),
                        })
                    })
                    .collect(),
            );
        }
        Err(_) => return None,
    };

    if surfaces.len() != jobs.len() {
        let reason = format!(
            "JPEG Metal batch returned {} surfaces for {} jobs",
            surfaces.len(),
            jobs.len()
        );
        return Some(
            (0..jobs.len())
                .map(|_| {
                    Err(WsiError::Unsupported {
                        reason: reason.clone(),
                    })
                })
                .collect(),
        );
    }

    Some(
        jobs.iter()
            .zip(surfaces)
            .map(|(job, surface)| match surface {
                Ok(surface) => tile_pixels_from_metal_jpeg_surface(surface, job, require_device),
                Err(err) if require_device => Err(WsiError::Unsupported {
                    reason: format!("JPEG Metal batch decode failed: {err}"),
                }),
                Err(_) => decode_one_jpeg_job(job).map(TilePixels::Cpu),
            })
            .collect(),
    )
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn decode_one_jpeg_pixels(
    job: &JpegDecodeJob<'_>,
    backend: J2kBackendRequest,
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    #[cfg(not(feature = "metal"))]
    let _ = metal_sessions;
    #[cfg(not(feature = "cuda"))]
    let _ = (backend, cuda_sessions);
    #[cfg(feature = "cuda")]
    if cuda_sessions.is_some() || matches!(backend, J2kBackendRequest::Cuda) {
        return decode_one_jpeg_pixels_cuda(job, require_device, cuda_sessions);
    }

    #[cfg(feature = "metal")]
    {
        return decode_one_jpeg_pixels_metal(job, require_device, metal_sessions);
    }

    #[allow(unreachable_code)]
    if require_device {
        Err(WsiError::Unsupported {
            reason: "device backend not available for jpeg".into(),
        })
    } else {
        decode_one_jpeg_job(job).map(TilePixels::Cpu)
    }
}

#[cfg(feature = "metal")]
fn decode_one_jpeg_pixels_metal(
    job: &JpegDecodeJob<'_>,
    require_device: bool,
    metal_sessions: MetalBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    let Some(metal_sessions) = metal_sessions else {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for jpeg without Metal session".into(),
            });
        }
        return decode_one_jpeg_job(job).map(TilePixels::Cpu);
    };
    let input = prepare_jpeg_input(
        job.data.as_ref(),
        job.tables.as_deref(),
        job.expected_width,
        job.expected_height,
        job.force_dimensions,
    );
    validate_j2k_jpeg_output_size(input.as_ref())?;
    let view = J2kJpegView::parse_with_options(
        input.as_ref(),
        J2kDecodeOptions::default().with_color_transform(job.color_transform),
    )
    .map_err(|err| WsiError::Jpeg(err.to_string()))?;
    if progressive_jpeg_requires_cpu_device_route(&view, require_device, "Metal")? {
        return decode_one_jpeg_job(job).map(TilePixels::Cpu);
    }
    let mut decoder =
        j2k_jpeg_metal::Decoder::from_view(view).map_err(|err| WsiError::Jpeg(err.to_string()))?;
    if metal_sessions.private_jpeg_decode() {
        match decoder.decode_private_rgb8_tile_with_session(metal_sessions.jpeg()) {
            Ok(tile) => {
                return Ok(TilePixels::Device(DeviceTile::Metal(
                    crate::output::metal::MetalDeviceTile::from_private_jpeg(tile)?,
                )));
            }
            Err(err) if require_device => {
                return Err(WsiError::Unsupported {
                    reason: format!("JPEG private Metal decode failed: {err}"),
                });
            }
            Err(_) => {}
        }
    }
    let surface = decoder
        .decode_to_device_with_session(J2kPixelFormat::Rgb8, metal_sessions.jpeg())
        .map_err(|err| WsiError::Jpeg(format!("j2k JPEG device decode failed: {err}")))?;

    tile_pixels_from_metal_jpeg_surface(surface, job, require_device)
}

#[cfg(feature = "metal")]
fn tile_pixels_from_metal_jpeg_surface(
    surface: j2k_jpeg_metal::Surface,
    job: &JpegDecodeJob<'_>,
    require_device: bool,
) -> Result<TilePixels, WsiError> {
    if surface.backend_kind() == J2kBackendKind::Metal {
        if surface.residency() == J2kJpegSurfaceResidency::CpuStagedMetalUpload {
            if require_device {
                return Err(WsiError::Unsupported {
                    reason:
                        "JPEG device decode produced CPU-staged Metal upload instead of resident Metal decode"
                            .into(),
                });
            }
            return decode_one_jpeg_job(job).map(TilePixels::Cpu);
        }
        if let Some(tile) = crate::output::metal::MetalDeviceTile::from_jpeg(surface)? {
            return Ok(TilePixels::Device(DeviceTile::Metal(tile)));
        }
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for jpeg".into(),
            });
        }
        return decode_one_jpeg_job(job).map(TilePixels::Cpu);
    }

    if require_device {
        return Err(WsiError::Unsupported {
            reason: "device backend not available for jpeg".into(),
        });
    }
    cpu_tile_from_jpeg_surface(surface, job.expected_width, job.expected_height)
        .map(TilePixels::Cpu)
}

#[cfg(feature = "cuda")]
fn decode_one_jpeg_pixels_cuda(
    job: &JpegDecodeJob<'_>,
    require_device: bool,
    cuda_sessions: CudaBackendSessionsRef<'_>,
) -> Result<TilePixels, WsiError> {
    let Some(cuda_sessions) = cuda_sessions else {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for jpeg without CUDA session".into(),
            });
        }
        return decode_one_jpeg_job(job).map(TilePixels::Cpu);
    };
    let input = prepare_jpeg_input(
        job.data.as_ref(),
        job.tables.as_deref(),
        job.expected_width,
        job.expected_height,
        job.force_dimensions,
    );
    validate_j2k_jpeg_output_size(input.as_ref())?;
    let view = J2kJpegView::parse_with_options(
        input.as_ref(),
        J2kDecodeOptions::default().with_color_transform(job.color_transform),
    )
    .map_err(|err| WsiError::Jpeg(err.to_string()))?;
    if progressive_jpeg_requires_cpu_device_route(&view, require_device, "CUDA")? {
        return decode_one_jpeg_job(job).map(TilePixels::Cpu);
    }

    let surface = cuda_sessions.with_jpeg(|session| {
        let mut decoder = j2k_jpeg_cuda::Decoder::from_view(view)
            .map_err(|err| WsiError::Jpeg(err.to_string()))?;
        decoder
            .submit_to_device(session, J2kPixelFormat::Rgb8, J2kBackendRequest::Cuda)
            .map_err(cuda_jpeg_decode_error)?
            .wait()
            .map_err(cuda_jpeg_decode_error)
    });

    match surface {
        Ok(surface) => tile_pixels_from_cuda_jpeg_surface(surface, job, require_device),
        Err(err) if require_device => Err(err),
        Err(_) => decode_one_jpeg_job(job).map(TilePixels::Cpu),
    }
}

#[cfg(feature = "cuda")]
fn tile_pixels_from_cuda_jpeg_surface(
    surface: j2k_jpeg_cuda::Surface,
    job: &JpegDecodeJob<'_>,
    require_device: bool,
) -> Result<TilePixels, WsiError> {
    if surface.backend_kind() != J2kBackendKind::Cuda {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "device backend not available for jpeg".into(),
            });
        }
        return decode_one_jpeg_job(job).map(TilePixels::Cpu);
    }
    let Some(cuda_surface) = surface.cuda_surface() else {
        if require_device {
            return Err(WsiError::Unsupported {
                reason: "JPEG CUDA decode returned CUDA backend without resident CUDA surface"
                    .into(),
            });
        }
        return decode_one_jpeg_job(job).map(TilePixels::Cpu);
    };
    let stats = cuda_surface.stats();
    if require_device && !stats.used_owned_cuda_decode() {
        return Err(WsiError::Unsupported {
            reason: "strict CUDA JPEG decode requires J2k-owned CUDA decode; nvJPEG or CPU-staged output is not accepted".into(),
        });
    }
    if stats.decode_path() == j2k_jpeg_cuda::CudaJpegDecodePath::None {
        if require_device {
            return Err(WsiError::Unsupported {
                reason:
                    "JPEG CUDA decode produced CPU-staged CUDA upload instead of owned CUDA decode"
                        .into(),
            });
        }
        return decode_one_jpeg_job(job).map(TilePixels::Cpu);
    }
    if let Some(tile) = crate::output::cuda::CudaDeviceTile::from_jpeg(surface)? {
        return Ok(TilePixels::Device(DeviceTile::Cuda(tile)));
    }
    if require_device {
        return Err(WsiError::Unsupported {
            reason: "device backend not available for jpeg".into(),
        });
    }
    decode_one_jpeg_job(job).map(TilePixels::Cpu)
}

#[cfg(feature = "cuda")]
fn cuda_jpeg_decode_error(err: j2k_jpeg_cuda::Error) -> WsiError {
    WsiError::Unsupported {
        reason: format!("JPEG CUDA device decode failed: {err}"),
    }
}

#[cfg(any(feature = "metal", feature = "cuda"))]
pub(super) fn progressive_jpeg_requires_cpu_device_route(
    view: &J2kJpegView<'_>,
    require_device: bool,
    backend_name: &str,
) -> Result<bool, WsiError> {
    if view.info().sof_kind != J2kSofKind::Progressive8 {
        return Ok(false);
    }
    if require_device {
        return Err(WsiError::Unsupported {
            reason: format!(
                "Progressive8 JPEG does not have a resident {backend_name} decode path; use CPU decode or a non-required device output preference"
            ),
        });
    }
    Ok(true)
}

#[cfg(feature = "metal")]
fn cpu_tile_from_jpeg_surface(
    surface: j2k_jpeg_metal::Surface,
    expected_width: u32,
    expected_height: u32,
) -> Result<CpuTile, WsiError> {
    if surface.pixel_format() != J2kPixelFormat::Rgb8 {
        return Err(WsiError::Jpeg(format!(
            "j2k JPEG returned unsupported pixel format {:?}",
            surface.pixel_format()
        )));
    }
    let (width, height) = surface.dimensions();
    let decoded = crop_jpeg_rgb_to_expected(
        DecodedJpegRgb {
            width,
            height,
            pixels: surface.as_bytes().to_vec(),
        },
        expected_width,
        expected_height,
    )?;
    CpuTile::from_u8_interleaved(
        decoded.width,
        decoded.height,
        3,
        ColorSpace::Rgb,
        decoded.pixels,
    )
}
