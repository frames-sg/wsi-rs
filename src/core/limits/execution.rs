//! Optional work may use only immediately available operation and slide headroom.
use super::*;
use std::sync::atomic::AtomicBool;
#[cfg(any(feature = "metal", feature = "cuda"))]
use std::sync::atomic::Ordering;

pub(crate) struct ReadExecutionContext<'a> {
    #[cfg(any(test, feature = "metal", feature = "cuda"))]
    reservation: &'a TransientReservation,
    #[cfg(any(test, feature = "metal", feature = "cuda"))]
    operation_limit: u64,
    #[cfg(any(test, feature = "metal", feature = "cuda"))]
    extra: Mutex<u64>,
    #[cfg(any(feature = "metal", feature = "cuda"))]
    calibration: &'a AtomicBool,
    pub(crate) control: Option<&'a ReadControl>,
}

impl<'a> ReadExecutionContext<'a> {
    pub(crate) fn new(
        reservation: &'a TransientReservation,
        operation_limit: u64,
        control: Option<&'a ReadControl>,
        calibration: &'a AtomicBool,
    ) -> Self {
        let _ = (reservation, operation_limit, calibration);
        Self {
            #[cfg(any(test, feature = "metal", feature = "cuda"))]
            reservation,
            #[cfg(any(test, feature = "metal", feature = "cuda"))]
            operation_limit,
            #[cfg(any(test, feature = "metal", feature = "cuda"))]
            extra: Mutex::new(0),
            #[cfg(any(feature = "metal", feature = "cuda"))]
            calibration,
            control,
        }
    }

    // Every context belonging to one public read borrows the same flag, including
    // contexts with different chunk reservations. Calibration can advance once;
    // selected routes remain free to execute every admitted batch.
    #[cfg(any(feature = "metal", feature = "cuda"))]
    pub(crate) fn claim_calibration(&self) -> bool {
        !self.calibration.swap(true, Ordering::Relaxed)
    }

    #[cfg(any(test, feature = "metal", feature = "cuda"))]
    pub(crate) fn try_extra(&self, bytes: u64) -> Result<Option<OptionalWork<'_>>, WsiError> {
        if let Some(control) = self.control {
            control.check_cancelled()?;
        }
        let mut extra = self.extra.lock().unwrap_or_else(|error| error.into_inner());
        if self
            .reservation
            .bytes
            .checked_add(*extra)
            .and_then(|n| n.checked_add(bytes))
            .is_none_or(|total| total > self.operation_limit)
        {
            return Ok(None);
        }
        let admission = &self.reservation.admission;
        let mut state = admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        advance_abandoned(&mut state);
        // Calibration never jumps an existing FIFO waiter and never waits while
        // holding the operation's ordinary reservation.
        if state.next_ticket != state.serving_ticket
            || state
                .in_flight
                .checked_add(bytes)
                .is_none_or(|total| total > admission.limit)
        {
            return Ok(None);
        }
        state.in_flight += bytes;
        *extra += bytes;
        Ok(Some(OptionalWork {
            reservation: TransientReservation {
                admission: Arc::clone(admission),
                bytes,
            },
            extra: &self.extra,
        }))
    }
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
pub(crate) struct OptionalWork<'a> {
    reservation: TransientReservation,
    extra: &'a Mutex<u64>,
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
impl Drop for OptionalWork<'_> {
    fn drop(&mut self) {
        let mut extra = self.extra.lock().unwrap_or_else(|error| error.into_inner());
        *extra -= self.reservation.bytes;
        // The reservation field releases slide admission after this method.
    }
}
