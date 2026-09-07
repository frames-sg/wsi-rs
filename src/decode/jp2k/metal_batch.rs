//! Operation-local native Metal grouping and ordered WSI materialization.
use std::collections::HashMap;
use std::sync::Arc;

use j2k::{
    BatchColor, BatchDecodeOptions, BatchItemError, BatchLayout, EncodedImage, NativeSampleType,
};
use j2k_metal::MetalBatchDecoder;

use super::metal_backend::{decode_prepared_jp2k_metal, resident_metal_jp2k_tile};
use super::prepare::{prepare_jp2k_job, PreparedJp2kJob};
use super::{Jp2kColorSpace, Jp2kDecodeJob};
use crate::core::execution_telemetry::{record, Event};
use crate::output::metal::{MetalBackendSessions, MetalDeviceTile};
use crate::WsiError;

type TileResult = Result<MetalDeviceTile, WsiError>;

// Larger concurrent native groups retained substantially more scratch and raised
// process RSS above the 10% gate. This is an execution target, not an output limit:
// an individually larger image still executes under the existing admission rules.
pub(super) const GROUP_OUTPUT_TARGET: u64 = 4 * 1024 * 1024;
// A byte target alone admits more small images and makes native workspace
// retention depend on that count. Keep both dimensions of the window bounded.
const MAX_WINDOW_IMAGES: usize = 16;

pub(super) fn decode_jobs(
    jobs: &[Jp2kDecodeJob<'_>],
    sessions: &MetalBackendSessions,
) -> Vec<TileResult> {
    decode_jobs_bounded(jobs, sessions, GROUP_OUTPUT_TARGET)
}

pub(super) fn decode_jobs_bounded(
    jobs: &[Jp2kDecodeJob<'_>],
    sessions: &MetalBackendSessions,
    output_target: u64,
) -> Vec<TileResult> {
    let mut metadata = Vec::with_capacity(jobs.len());
    let mut output = Vec::with_capacity(jobs.len());
    let mut inputs = Vec::new();
    let mut slots = Vec::new();
    // Duplicate requests sharing immutable input bytes share their owned codec
    // input too. Pointer identity is valid only within this borrowing operation.
    let mut owned = HashMap::new();
    for (slot, job) in jobs.iter().enumerate() {
        match prepare_jp2k_job(job) {
            Ok(prepared) => {
                let key = (prepared.input.as_ptr(), prepared.input.len());
                let bytes = owned
                    .entry(key)
                    .or_insert_with(|| Arc::<[u8]>::from(prepared.input));
                inputs.push(EncodedImage::full(bytes.clone()));
                slots.push(slot);
                metadata.push(Some(prepared));
                output.push(None);
            }
            Err(error) => {
                metadata.push(None);
                output.push(Some(Err(error)));
            }
        }
    }
    let mut inputs = inputs.into_iter();
    let mut start = 0;
    while start < slots.len() {
        let end = bounded_end(&metadata, &slots, start, output_target);
        let group_slots = &slots[start..end];
        let result = decode_groups(
            inputs.by_ref().take(end - start).collect(),
            group_slots,
            &metadata,
            &mut output,
            sessions,
        );
        if let Err(error) = result {
            let reason = error.to_string();
            for slot in group_slots {
                if output[*slot].is_none() {
                    output[*slot] = Some(Err(WsiError::Jp2k(reason.clone())));
                }
            }
        }
        start = end;
    }
    convert_outputs(&metadata, &mut output, sessions);
    output
        .into_iter()
        .map(|tile| {
            tile.unwrap_or_else(|| {
                Err(WsiError::Jp2k(
                    "Metal batch did not return the requested source index".into(),
                ))
            })
        })
        .map(|tile| tile.and_then(|tile| sessions.retain_readback_queue(tile)))
        .collect()
}

fn decode_groups(
    inputs: Vec<EncodedImage>,
    slots: &[usize],
    metadata: &[Option<PreparedJp2kJob<'_>>],
    output: &mut [Option<TileResult>],
    sessions: &MetalBackendSessions,
) -> Result<(), WsiError> {
    let mut decoder = MetalBatchDecoder::with_backend_session_and_options(
        sessions.j2k().clone(),
        BatchDecodeOptions {
            layout: BatchLayout::Nhwc,
            workers: std::num::NonZeroUsize::new(
                crate::core::decode_runtime::DecodeRuntime::default_arc().cpu_worker_count(),
            ),
            ..BatchDecodeOptions::default()
        },
    );
    let prepared = decoder
        .prepare(inputs)
        .map_err(|error| WsiError::Jp2k(error.to_string()))?;
    execute_groups(&prepared, slots, metadata, output, sessions, &mut decoder)
}

pub(super) fn execute_prepared_bounded(
    prepared: &j2k::PreparedBatch,
    slots: &[usize],
    metadata: &[Option<PreparedJp2kJob<'_>>],
    output: &mut [Option<TileResult>],
    sessions: &MetalBackendSessions,
    output_target: u64,
) -> Result<(), WsiError> {
    let total = prepared.groups().iter().fold(0_u64, |sum, group| {
        sum.saturating_add(
            u64::from(group.info().dimensions.0)
                .saturating_mul(u64::from(group.info().dimensions.1))
                .saturating_mul(4)
                .saturating_mul(group.images().len() as u64),
        )
    });
    if total <= output_target && slots.len() <= MAX_WINDOW_IMAGES {
        let mut decoder = MetalBatchDecoder::with_backend_session_and_options(
            sessions.j2k().clone(),
            prepared.options(),
        );
        return execute_groups(prepared, slots, metadata, output, sessions, &mut decoder);
    }
    // Clone only retained image owners. Regrouping preserves strict validation
    // and reuses their native offset plans without parsing encoded bytes again.
    let mut images = prepared
        .groups()
        .iter()
        .flat_map(|group| {
            group
                .source_indices()
                .iter()
                .copied()
                .zip(group.images().iter().cloned())
        })
        .collect::<Vec<_>>();
    images.sort_unstable_by_key(|(index, _)| *index);
    let bounded_slots = images
        .iter()
        .map(|(index, _)| slots[*index])
        .collect::<Vec<_>>();
    let mut images = images.into_iter().map(|(_, image)| image);
    let mut start = 0;
    while start < bounded_slots.len() {
        let end = bounded_end(metadata, &bounded_slots, start, output_target);
        let batch = j2k::prepare_batch_from_images(
            images.by_ref().take(end - start).collect(),
            prepared.options(),
        )
        .map_err(|error| WsiError::Jp2k(error.to_string()))?;
        let mut decoder = MetalBatchDecoder::with_backend_session_and_options(
            sessions.j2k().clone(),
            prepared.options(),
        );
        execute_groups(
            &batch,
            &bounded_slots[start..end],
            metadata,
            output,
            sessions,
            &mut decoder,
        )?;
        start = end;
    }
    apply_indexed_errors(prepared.errors(), slots, metadata, output, sessions);
    Ok(())
}

pub(super) fn execute_groups(
    prepared: &j2k::PreparedBatch,
    slots: &[usize],
    metadata: &[Option<PreparedJp2kJob<'_>>],
    output: &mut [Option<TileResult>],
    sessions: &MetalBackendSessions,
    decoder: &mut MetalBatchDecoder,
) -> Result<(), WsiError> {
    record(Event::MetalBatchSubmissions, 1);
    let pending = decoder
        .submit_prepared(prepared)
        .map_err(|error| WsiError::Jp2k(error.to_string()))?;
    record(
        Event::MetalBatchGroups,
        decoder
            .submissions()
            .map_err(|error| WsiError::Jp2k(error.to_string()))? as usize,
    );
    record(Event::MetalCompletionWaits, 1);
    let (groups, errors, group_errors) = pending
        .wait()
        .map_err(|error| WsiError::Jp2k(error.to_string()))?
        .into_parts();
    apply_indexed_errors(&errors, slots, metadata, output, sessions);
    for error in group_errors {
        let (indices, error) = error.into_parts();
        let capability_rejection = matches!(
            error,
            j2k_metal::Error::UnsupportedMetalRequest { .. }
                | j2k_metal::Error::MetalDirectFallback { .. }
        );
        for index in indices {
            let slot = slots[index];
            output[slot] = Some(if capability_rejection {
                strict_single_without_conversion(
                    metadata[slot].as_ref().expect("validated input"),
                    sessions,
                )
            } else {
                Err(WsiError::Jp2k(error.to_string()))
            });
        }
    }
    for group in groups {
        let (info, indices, _, _, surfaces) = group.into_parts();
        if info.layout != BatchLayout::Nhwc
            || info.color != BatchColor::Rgb
            || info.sample_type != NativeSampleType::U8
            || info.precision != 8
            || info.signed
            || indices.len() != surfaces.len()
        {
            for index in indices {
                output[slots[index]] = Some(Err(WsiError::Jp2k(
                    "Metal batch violated unsigned NHWC RGB8 output contract".into(),
                )));
            }
            continue;
        }
        for (index, surface) in indices.into_iter().zip(surfaces) {
            let slot = slots[index];
            let job = metadata[slot].as_ref().expect("validated input");
            output[slot] = Some(
                resident_metal_jp2k_tile(surface)
                    .and_then(|tile| tile.crop_top_left(job.expected_width, job.expected_height)),
            );
        }
    }
    #[cfg(feature = "route-telemetry")]
    crate::core::execution_telemetry::record_metal_pools(sessions);
    Ok(())
}

fn bounded_end(
    metadata: &[Option<PreparedJp2kJob<'_>>],
    slots: &[usize],
    start: usize,
    output_target: u64,
) -> usize {
    let mut end = start;
    let mut bytes = 0_u64;
    while end < slots.len() && end - start < MAX_WINDOW_IMAGES {
        let job = metadata[slots[end]].as_ref().expect("validated input");
        let next = u64::from(job.decoded_width)
            .saturating_mul(u64::from(job.decoded_height))
            .saturating_mul(4);
        if end > start && bytes.saturating_add(next) > output_target {
            break;
        }
        bytes = bytes.saturating_add(next);
        end += 1;
    }
    end
}

fn apply_indexed_errors(
    errors: &[j2k::IndexedBatchError],
    slots: &[usize],
    metadata: &[Option<PreparedJp2kJob<'_>>],
    output: &mut [Option<TileResult>],
    sessions: &MetalBackendSessions,
) {
    for error in errors {
        let slot = slots[error.index];
        output[slot] = Some(
            if matches!(
                error.source,
                BatchItemError::NonRepresentableBatchOutput { .. }
            ) {
                // Only representability rejection may use the strict single-image
                // Metal path. Parse/decode failures retain their per-input error.
                strict_single_without_conversion(
                    metadata[slot].as_ref().expect("validated input"),
                    sessions,
                )
            } else {
                Err(WsiError::Jp2k(error.to_string()))
            },
        );
    }
}

fn strict_single_without_conversion(
    job: &PreparedJp2kJob<'_>,
    sessions: &MetalBackendSessions,
) -> TileResult {
    // Conversion is shared with the successful native groups after all crops.
    let mut raw = *job;
    raw.output_colorspace = Jp2kColorSpace::Rgb;
    decode_prepared_jp2k_metal(&raw, sessions)
}

pub(super) fn convert_outputs(
    metadata: &[Option<PreparedJp2kJob<'_>>],
    output: &mut [Option<TileResult>],
    sessions: &MetalBackendSessions,
) {
    let mut indices = Vec::new();
    let mut tiles = Vec::new();
    for (index, (job, result)) in metadata.iter().zip(output.iter()).enumerate() {
        if job
            .as_ref()
            .is_some_and(|job| job.output_colorspace == Jp2kColorSpace::YCbCr)
        {
            if let Some(Ok(tile)) = result {
                indices.push(index);
                tiles.push(tile.clone());
            }
        }
    }
    if tiles.is_empty() {
        return;
    }
    match sessions.ycbcr8_tiles_to_rgb8(&tiles) {
        Ok(converted) if converted.len() == indices.len() => {
            for (index, tile) in indices.into_iter().zip(converted) {
                output[index] = Some(Ok(tile));
            }
        }
        result => {
            let reason = result.err().map_or_else(
                || "Metal converter returned wrong tile count".into(),
                |error| error.to_string(),
            );
            for index in indices {
                output[index] = Some(Err(WsiError::Jp2k(reason.clone())));
            }
        }
    }
}
