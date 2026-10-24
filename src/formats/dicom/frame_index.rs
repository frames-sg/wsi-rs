use std::path::Path;

use crate::error::WsiError;

pub(super) mod batch_io;
pub(super) mod model;
pub(super) mod offset_tables;
pub(super) mod raw_little_endian;
mod token_stream;
pub(super) mod validation;

pub(super) use batch_io::{
    frame_read_span, group_frame_read_spans, read_encapsulated_fragments,
    read_encapsulated_frame_group, DicomFrameReadSpan,
};
pub(super) use model::DicomEncapsulatedFrames;
pub(super) use validation::preflight_compressed_frame_with_limit;

pub(super) fn scan_encapsulated_frames_controlled(
    path: &Path,
    transfer_syntax_uid: &str,
    number_of_frames: u32,
    control: Option<&crate::ReadControl>,
) -> Result<DicomEncapsulatedFrames, WsiError> {
    check_index_control(control)?;
    let fast_started = index_diagnostic_timer(control);
    match raw_little_endian::scan_encapsulated_frames_raw_little_endian_controlled(
        path,
        number_of_frames,
        control,
    ) {
        Ok(Some(index)) => {
            let elapsed = index_elapsed(fast_started);
            record_index_diagnostic(
                control,
                crate::DicomIndexOutcome::BuiltFast {
                    mapping: index.mapping,
                },
                elapsed,
            );
            if let Some(elapsed) = elapsed {
                tracing::debug!(
                    path = %path.display(),
                    strategy = ?index.mapping,
                    elapsed_us = elapsed.as_micros(),
                    "built DICOM encapsulated frame index"
                );
            }
            return Ok(index.frames);
        }
        Ok(None) => {
            let elapsed = index_elapsed(fast_started);
            record_index_diagnostic(control, crate::DicomIndexOutcome::FastPathFallback, elapsed);
        }
        Err(WsiError::Cancelled) => return Err(WsiError::Cancelled),
        Err(error) => {
            let elapsed = index_elapsed(fast_started);
            record_index_diagnostic(control, crate::DicomIndexOutcome::FastPathFallback, elapsed);
            tracing::debug!(
                path = %path.display(),
                error = %error,
                "fast DICOM encapsulated frame indexing failed; using token parser"
            );
        }
    }

    check_index_control(control)?;
    let token_started = index_diagnostic_timer(control);
    let frames = token_stream::scan_encapsulated_frames_tokenized(
        path,
        transfer_syntax_uid,
        number_of_frames,
        control,
    )?;
    let elapsed = index_elapsed(token_started);
    record_index_diagnostic(control, crate::DicomIndexOutcome::TokenFallback, elapsed);
    if let Some(elapsed) = elapsed {
        tracing::debug!(
            path = %path.display(),
            strategy = "token_fallback",
            elapsed_us = elapsed.as_micros(),
            "built DICOM encapsulated frame index"
        );
    }
    Ok(frames)
}

fn index_diagnostic_timer(control: Option<&crate::ReadControl>) -> Option<std::time::Instant> {
    index_diagnostic_timer_with(
        control,
        tracing::enabled!(tracing::Level::DEBUG),
        std::time::Instant::now,
    )
}

pub(super) fn index_diagnostic_timer_with(
    control: Option<&crate::ReadControl>,
    tracing_enabled: bool,
    clock: impl FnOnce() -> std::time::Instant,
) -> Option<std::time::Instant> {
    (control.is_some_and(crate::ReadControl::diagnostics_enabled) || tracing_enabled).then(clock)
}

fn index_elapsed(started: Option<std::time::Instant>) -> Option<std::time::Duration> {
    started.map(|started| started.elapsed())
}

fn record_index_diagnostic(
    control: Option<&crate::ReadControl>,
    outcome: crate::DicomIndexOutcome,
    elapsed: Option<std::time::Duration>,
) {
    if let (Some(control), Some(elapsed)) = (control, elapsed) {
        control.record_diagnostic(crate::DicomIndexDiagnostic::new(outcome, elapsed));
    }
}

fn check_index_control(control: Option<&crate::ReadControl>) -> Result<(), WsiError> {
    control.map_or(Ok(()), crate::ReadControl::check_cancelled)
}

pub(super) const PIXEL_DATA_TAG_LE: [u8; 4] = [0xE0, 0x7F, 0x10, 0x00];
pub(super) const EXTENDED_OFFSET_TABLE_TAG_LE: [u8; 4] = [0xE0, 0x7F, 0x01, 0x00];
pub(super) const EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE: [u8; 4] = [0xE0, 0x7F, 0x02, 0x00];
pub(super) const DICOM_ITEM_TAG_LE: [u8; 4] = [0xFE, 0xFF, 0x00, 0xE0];
pub(super) const DICOM_SEQUENCE_DELIMITER_TAG_LE: [u8; 4] = [0xFE, 0xFF, 0xDD, 0xE0];
pub(super) const EXPLICIT_VR_LONG_HEADER_LEN: usize = 12;

/// Whether an explicit-VR element with this VR has the long header form: two
/// reserved bytes and a 32-bit value length (PS3.5 Section 7.1.2).
pub(super) fn explicit_vr_uses_u32_length(vr: [u8; 2]) -> bool {
    matches!(
        &vr,
        b"OB"
            | b"OD"
            | b"OF"
            | b"OL"
            | b"OV"
            | b"OW"
            | b"SQ"
            | b"SV"
            | b"UC"
            | b"UN"
            | b"UR"
            | b"UT"
            | b"UV"
    )
}
