use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::{ReadControl, WsiError};

pub(crate) const MAX_COMPRESSED_INPUT_BYTES: u64 = 128 * 1024 * 1024;
pub(crate) const MAX_DECODED_IMAGE_BYTES: u64 = 128 * 1024 * 1024;

const MIB: u64 = 1024 * 1024;

/// Per-slide limits for untrusted metadata, decoded output, and transient work.
///
/// Defaults are 128 MiB aggregate metadata, 16 MiB per metadata value,
/// 128 MiB each for indexes, encoded units, and decoded outputs, 33,554,432
/// pixels/128 MiB RGBA per region, 384 MiB transient work per operation,
/// 512 MiB in flight per slide, and a 256 MiB batch-chunk target. Every
/// `with_*` builder rejects zero; smaller values are useful at service trust
/// boundaries. Batch limits preserve order and cardinality by chunking, while
/// a single oversized tile, associated image, or region returns
/// [`WsiError::ResourceLimit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SlideLimits {
    aggregate_metadata_bytes: u64,
    metadata_value_bytes: u64,
    tile_index_bytes: u64,
    encoded_unit_bytes: u64,
    decoded_output_bytes: u64,
    region_pixels: u64,
    region_rgba_bytes: u64,
    operation_transient_bytes: u64,
    slide_transient_bytes: u64,
    batch_chunk_bytes: u64,
}

impl SlideLimits {
    pub(crate) const fn default_const() -> Self {
        Self {
            aggregate_metadata_bytes: 128 * MIB,
            metadata_value_bytes: 16 * MIB,
            tile_index_bytes: 128 * MIB,
            encoded_unit_bytes: 128 * MIB,
            decoded_output_bytes: 128 * MIB,
            region_pixels: 33_554_432,
            region_rgba_bytes: 128 * MIB,
            operation_transient_bytes: 384 * MIB,
            slide_transient_bytes: 512 * MIB,
            batch_chunk_bytes: 256 * MIB,
        }
    }

    pub const fn aggregate_metadata_bytes(self) -> u64 {
        self.aggregate_metadata_bytes
    }
    pub const fn metadata_value_bytes(self) -> u64 {
        self.metadata_value_bytes
    }
    pub const fn tile_index_bytes(self) -> u64 {
        self.tile_index_bytes
    }
    pub const fn encoded_unit_bytes(self) -> u64 {
        self.encoded_unit_bytes
    }
    pub const fn decoded_output_bytes(self) -> u64 {
        self.decoded_output_bytes
    }
    pub const fn region_pixels(self) -> u64 {
        self.region_pixels
    }
    pub const fn region_rgba_bytes(self) -> u64 {
        self.region_rgba_bytes
    }
    pub const fn operation_transient_bytes(self) -> u64 {
        self.operation_transient_bytes
    }
    pub const fn slide_transient_bytes(self) -> u64 {
        self.slide_transient_bytes
    }
    pub const fn batch_chunk_bytes(self) -> u64 {
        self.batch_chunk_bytes
    }

    pub fn with_aggregate_metadata_bytes(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.aggregate_metadata_bytes = checked_limit("aggregate metadata", value)?;
        Ok(self)
    }
    pub fn with_metadata_value_bytes(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.metadata_value_bytes = checked_limit("metadata value", value)?;
        Ok(self)
    }
    pub fn with_tile_index_bytes(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.tile_index_bytes = checked_limit("tile/frame index", value)?;
        Ok(self)
    }
    pub fn with_encoded_unit_bytes(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.encoded_unit_bytes = checked_limit("encoded tile/frame unit", value)?;
        Ok(self)
    }
    pub fn with_decoded_output_bytes(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.decoded_output_bytes = checked_limit("decoded output", value)?;
        Ok(self)
    }
    pub fn with_region_pixels(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.region_pixels = checked_limit("region pixels", value)?;
        Ok(self)
    }
    pub fn with_region_rgba_bytes(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.region_rgba_bytes = checked_limit("region RGBA", value)?;
        Ok(self)
    }
    pub fn with_operation_transient_bytes(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.operation_transient_bytes = checked_limit("operation transient", value)?;
        Ok(self)
    }
    pub fn with_slide_transient_bytes(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.slide_transient_bytes = checked_limit("slide transient", value)?;
        Ok(self)
    }
    pub fn with_batch_chunk_bytes(mut self, value: u64) -> Result<Self, SlideLimitError> {
        self.batch_chunk_bytes = checked_limit("batch chunk", value)?;
        Ok(self)
    }

    pub(crate) fn with_region_pixels_compat(mut self, value: u64) -> Self {
        self.region_pixels = value;
        self
    }
}

impl Default for SlideLimits {
    fn default() -> Self {
        Self::default_const()
    }
}

/// Peak transient work promised by a reader for one decode operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadWork {
    pub(crate) encoded: u64,
    pub(crate) decoded: u64,
}

impl ReadWork {
    pub(crate) const fn new(encoded: u64, decoded: u64) -> Self {
        Self { encoded, decoded }
    }

    /// Normal decode work includes the encoded input and two decoded-size
    /// buffers (codec/staging plus the caller-visible output).
    pub(crate) fn ordinary_bytes(self, limit: u64) -> Result<u64, WsiError> {
        let requested = self
            .decoded
            .checked_mul(2)
            .and_then(|decoded| self.encoded.checked_add(decoded))
            .unwrap_or(u64::MAX);
        if requested > limit {
            return Err(WsiError::ResourceLimit {
                resource: "per-operation transient work",
                requested,
                limit,
            });
        }
        Ok(requested)
    }

    pub(crate) fn encoded_only_bytes(self, limit: u64) -> Result<u64, WsiError> {
        if self.encoded > limit {
            return Err(WsiError::ResourceLimit {
                resource: "per-operation transient work",
                requested: self.encoded,
                limit,
            });
        }
        Ok(self.encoded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlideLimitError {
    field: &'static str,
}

impl fmt::Display for SlideLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{0} limit must be greater than zero", self.field)
    }
}

impl std::error::Error for SlideLimitError {}

fn checked_limit(field: &'static str, value: u64) -> Result<u64, SlideLimitError> {
    if value == 0 {
        Err(SlideLimitError { field })
    } else {
        Ok(value)
    }
}

#[derive(Debug)]
struct AdmissionState {
    in_flight: u64,
    next_ticket: u64,
    serving_ticket: u64,
    abandoned: BTreeSet<u64>,
}

/// One FIFO admission queue per open slide.
#[derive(Debug)]
pub(crate) struct SlideAdmission {
    limit: u64,
    state: Mutex<AdmissionState>,
    changed: Condvar,
}

impl SlideAdmission {
    pub(crate) fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            limit,
            state: Mutex::new(AdmissionState {
                in_flight: 0,
                next_ticket: 0,
                serving_ticket: 0,
                abandoned: BTreeSet::new(),
            }),
            changed: Condvar::new(),
        })
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        bytes: u64,
        control: Option<&ReadControl>,
    ) -> Result<TransientReservation, WsiError> {
        if bytes > self.limit {
            return Err(WsiError::ResourceLimit {
                resource: "per-slide in-flight transient work",
                requested: bytes,
                limit: self.limit,
            });
        }
        if let Some(control) = control {
            control.check_cancelled()?;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.wrapping_add(1);
        loop {
            advance_abandoned(&mut state);
            if ticket == state.serving_ticket
                && state
                    .in_flight
                    .checked_add(bytes)
                    .is_some_and(|total| total <= self.limit)
            {
                state.in_flight += bytes;
                state.serving_ticket = state.serving_ticket.wrapping_add(1);
                advance_abandoned(&mut state);
                self.changed.notify_all();
                return Ok(TransientReservation {
                    admission: Arc::clone(self),
                    bytes,
                });
            }
            if control.is_some_and(|control| control.check_cancelled().is_err()) {
                state.abandoned.insert(ticket);
                advance_abandoned(&mut state);
                self.changed.notify_all();
                return Err(WsiError::Cancelled);
            }
            if control.is_some() {
                let waited = self.changed.wait_timeout(state, Duration::from_millis(10));
                state = waited.unwrap_or_else(|error| error.into_inner()).0;
            } else {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
        }
    }
}

fn advance_abandoned(state: &mut AdmissionState) {
    while state.abandoned.remove(&state.serving_ticket) {
        state.serving_ticket = state.serving_ticket.wrapping_add(1);
    }
}

pub(crate) struct TransientReservation {
    admission: Arc<SlideAdmission>,
    bytes: u64,
}

impl Drop for TransientReservation {
    fn drop(&mut self) {
        let mut state = self
            .admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.in_flight = state.in_flight.saturating_sub(self.bytes);
        self.admission.changed.notify_all();
    }
}

pub(crate) fn checked_product_to_usize(
    factors: &[u64],
    max: u64,
    label: &str,
) -> Result<usize, String> {
    let value = factors
        .iter()
        .try_fold(1_u64, |product, factor| product.checked_mul(*factor));
    let Some(value) = value else {
        return Err(format!("{label} length overflow"));
    };
    if value > max {
        return Err(format!("{label} exceeds {max} byte safety limit"));
    }
    usize::try_from(value).map_err(|_| format!("{label} is not addressable on this platform"))
}

pub(crate) fn read_to_end_bounded(reader: impl Read, max: u64, label: &str) -> io::Result<Vec<u8>> {
    let allocation = usize::try_from(max.min(1024 * 1024)).unwrap_or(0);
    let mut output = Vec::with_capacity(allocation);
    let mut limited = reader.take(max.saturating_add(1));
    limited.read_to_end(&mut output)?;
    if u64::try_from(output.len()).unwrap_or(u64::MAX) > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} exceeds {max} byte safety limit"),
        ));
    }
    Ok(output)
}

pub(crate) fn read_file_bounded(path: &Path, max: u64, label: &str) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    read_to_end_bounded(file, max, label)
}

#[cfg(test)]
mod tests;
