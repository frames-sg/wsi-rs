use j2k_core::{DeviceSurface as J2kDeviceSurface, PixelFormat as J2kPixelFormat};

use super::prepare::PreparedJp2kJob;
use super::Jp2kColorSpace;
use crate::error::WsiError;

type TileResult = Result<crate::output::cuda::CudaDeviceTile, WsiError>;

pub(super) fn decode_prepared_jobs(
    jobs: Vec<Result<PreparedJp2kJob<'_>, WsiError>>,
    sessions: &crate::output::cuda::CudaBackendSessions,
) -> Vec<TileResult> {
    let mut output = Vec::with_capacity(jobs.len());
    let mut jobs = jobs.into_iter().peekable();
    while let Some(job) = jobs.next() {
        let first = match job {
            Ok(job) => job,
            Err(error) => {
                output.push(Err(error));
                continue;
            }
        };
        if first.output_colorspace == Jp2kColorSpace::YCbCr {
            output.push(decode_prepared_jp2k_cuda(&first, sessions));
            continue;
        }
        let mut bytes = first.output_len;
        let mut group = vec![first];
        // Use the same 4 MiB execution target as Metal while j2k 0.12 plans
        // encoded inputs in parallel and reuses pinned uploads and output pools.
        while let Some(Ok(next)) = jobs.peek() {
            if group.len() == 16
                || next.output_colorspace != Jp2kColorSpace::Rgb
                || bytes.saturating_add(next.output_len) > 4 * 1024 * 1024
            {
                break;
            }
            bytes += next.output_len;
            group.push(*next);
            jobs.next();
        }
        output.extend(decode_group(&group, sessions));
    }
    output
}

fn decode_group(
    jobs: &[PreparedJp2kJob<'_>],
    sessions: &crate::output::cuda::CudaBackendSessions,
) -> Vec<TileResult> {
    if jobs.len() == 1 {
        return vec![decode_prepared_jp2k_cuda(&jobs[0], sessions)];
    }
    let inputs = jobs.iter().map(|job| job.input).collect::<Vec<_>>();
    let result = sessions.with_j2k(|session| {
        Ok(j2k_cuda::J2kDecoder::decode_batch_to_device_with_session(
            &inputs,
            J2kPixelFormat::Rgb8,
            session,
        ))
    });
    let error = match result {
        Ok(Ok(surfaces)) if surfaces.len() == jobs.len() => {
            return surfaces
                .into_iter()
                .zip(jobs)
                .map(|(surface, job)| {
                    cuda_tile_from_jp2k_surface(
                        surface,
                        job.expected_width,
                        job.expected_height,
                        job.output_colorspace,
                    )
                })
                .collect();
        }
        Ok(Ok(_)) => "CUDA batch did not return every requested image".to_owned(),
        Ok(Err(error)) if !error.session_is_unusable() => {
            // A malformed or unsupported tile must not suppress neighboring
            // results. Retry only recoverable groups through strict CUDA decode.
            return jobs
                .iter()
                .map(|job| decode_prepared_jp2k_cuda(job, sessions))
                .collect();
        }
        Ok(Err(error)) => error.to_string(),
        Err(error) => error.to_string(),
    };
    jobs.iter()
        .map(|_| {
            Err(WsiError::Unsupported {
                reason: format!("strict JP2K CUDA batch decode failed: {error}"),
            })
        })
        .collect()
}

pub(super) fn decode_prepared_jp2k_cuda(
    job: &PreparedJp2kJob<'_>,
    sessions: &crate::output::cuda::CudaBackendSessions,
) -> Result<crate::output::cuda::CudaDeviceTile, WsiError> {
    let surface = sessions.with_j2k(|session| {
        let mut decoder =
            j2k_cuda::J2kDecoder::new(job.input).map_err(|err| WsiError::Jp2k(err.to_string()))?;
        decoder
            .decode_to_device_with_session(J2kPixelFormat::Rgb8, session)
            .map_err(cuda_jp2k_decode_error)
    })?;
    cuda_tile_from_jp2k_surface(
        surface,
        job.expected_width,
        job.expected_height,
        job.output_colorspace,
    )
}

fn cuda_tile_from_jp2k_surface(
    surface: j2k_cuda::Surface,
    expected_width: u32,
    expected_height: u32,
    colorspace: Jp2kColorSpace,
) -> Result<crate::output::cuda::CudaDeviceTile, WsiError> {
    if surface.backend_kind() != j2k_core::BackendKind::Cuda {
        return Err(WsiError::Unsupported {
            reason: "strict JP2K CUDA decode returned a host surface".into(),
        });
    }
    if surface.residency() != j2k_cuda::SurfaceResidency::CudaResidentDecode
        || surface.cuda_surface().is_none()
    {
        return Err(WsiError::Unsupported {
            reason: "strict JP2K CUDA decode did not return a resident CUDA surface".into(),
        });
    }
    if colorspace == Jp2kColorSpace::YCbCr {
        return Err(WsiError::Unsupported {
            reason: "strict JP2K CUDA YCbCr output requires a resident CUDA RGB conversion".into(),
        });
    }
    crate::output::cuda::CudaDeviceTile::from_j2k(surface, expected_width, expected_height)?
        .ok_or_else(|| WsiError::Unsupported {
            reason: "strict JP2K CUDA decode did not produce a public resident tile".into(),
        })
}

fn cuda_jp2k_decode_error(err: j2k_cuda::Error) -> WsiError {
    WsiError::Unsupported {
        reason: format!("strict JP2K CUDA device decode failed: {err}"),
    }
}
