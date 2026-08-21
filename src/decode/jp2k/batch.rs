use std::num::NonZeroUsize;

use j2k::{
    decode_tiles_into as j2k_decode_jp2k_tiles_into, CpuDecodeParallelism,
    TileBatchOptions as J2kTileBatchOptions, TileDecodeJob as J2kJp2kTileDecodeJob,
};
use j2k_core::{BackendRequest as J2kBackendRequest, PixelFormat as J2kPixelFormat};

#[cfg(test)]
use super::cpu::decode_one_jp2k_job;
use super::cpu::decode_prepared_jp2k_job;
use super::output::sample_buffer_from_rgb8_bytes;
use super::prepare::{prepare_jp2k_job, PreparedJp2kJob};
use super::{Jp2kColorSpace, Jp2kDecodeJob};
use crate::core::types::CpuTile;
use crate::error::WsiError;

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
    if jobs.is_empty() {
        return Vec::new();
    }
    let prepared = jobs.iter().map(prepare_jp2k_job).collect::<Vec<_>>();
    if prepared.iter().all(Result::is_ok) {
        let prepared_jobs = prepared
            .iter()
            .map(|result| *result.as_ref().expect("all preparation results checked"))
            .collect::<Vec<_>>();
        if let Some(decoded) = try_decode_prepared_batch_jp2k_with_j2k(&prepared_jobs) {
            return decoded.into_iter().map(Ok).collect();
        }
    }

    use rayon::prelude::*;
    prepared
        .into_par_iter()
        .map(|prepared| {
            prepared
                .and_then(|prepared| {
                    decode_prepared_jp2k_job(&prepared, CpuDecodeParallelism::Serial)
                })
                .map_err(|error| WsiError::Codec {
                    codec: "j2k",
                    source: Box::new(error),
                })
        })
        .collect()
}

pub(super) struct PreparedJp2kBatchJob {
    pub(super) decoded_width: u32,
    pub(super) decoded_height: u32,
    pub(super) expected_width: u32,
    pub(super) expected_height: u32,
    pub(super) output_colorspace: Jp2kColorSpace,
}

#[cfg(test)]
pub(super) fn try_decode_batch_jp2k_with_j2k(jobs: &[Jp2kDecodeJob<'_>]) -> Option<Vec<CpuTile>> {
    let prepared = jobs
        .iter()
        .map(prepare_jp2k_job)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    try_decode_prepared_batch_jp2k_with_j2k(&prepared)
}

fn try_decode_prepared_batch_jp2k_with_j2k(
    prepared: &[PreparedJp2kJob<'_>],
) -> Option<Vec<CpuTile>> {
    if prepared.len() <= 1 {
        return None;
    }

    for job in prepared {
        if !matches!(
            job.backend,
            J2kBackendRequest::Auto | J2kBackendRequest::Cpu
        ) {
            return None;
        }
    }

    let mut outputs = prepared
        .iter()
        .map(|job| vec![0u8; job.output_len])
        .collect::<Vec<_>>();
    let mut batch_jobs = prepared
        .iter()
        .zip(outputs.iter_mut())
        .map(|(prepared, output)| J2kJp2kTileDecodeJob {
            input: prepared.input,
            out: output.as_mut_slice(),
            stride: prepared.row_bytes,
        })
        .collect::<Vec<_>>();

    j2k_decode_jp2k_tiles_into(
        &mut batch_jobs,
        J2kPixelFormat::Rgb8,
        J2kTileBatchOptions {
            workers: NonZeroUsize::new(rayon::current_num_threads()),
        },
    )
    .ok()?;
    drop(batch_jobs);

    let materialization = prepared
        .iter()
        .map(|job| PreparedJp2kBatchJob {
            decoded_width: job.decoded_width,
            decoded_height: job.decoded_height,
            expected_width: job.expected_width,
            expected_height: job.expected_height,
            output_colorspace: job.output_colorspace,
        })
        .collect();
    materialize_jp2k_batch_outputs(materialization, outputs).ok()
}

pub(super) fn materialize_jp2k_batch_outputs(
    prepared: Vec<PreparedJp2kBatchJob>,
    outputs: Vec<Vec<u8>>,
) -> Result<Vec<CpuTile>, WsiError> {
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
}

#[cfg(test)]
pub(super) fn decode_jp2k_tile_batch_with_j2k(
    reqs: &[Jp2kDecodeJob<'_>],
) -> Result<Vec<CpuTile>, WsiError> {
    reqs.iter().map(decode_one_jp2k_job).collect()
}
