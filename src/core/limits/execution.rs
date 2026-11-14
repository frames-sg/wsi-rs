//! Optional work may use only immediately available operation and slide headroom.
use super::*;

pub(crate) struct ReadExecutionContext<'a> {
    reservation: &'a TransientReservation,
    operation_limit: u64,
    extra: Mutex<u64>,
    pub(crate) control: Option<&'a ReadControl>,
}

impl<'a> ReadExecutionContext<'a> {
    pub(crate) fn new(
        reservation: &'a TransientReservation,
        operation_limit: u64,
        control: Option<&'a ReadControl>,
    ) -> Self {
        Self {
            reservation,
            operation_limit,
            extra: Mutex::new(0),
            control,
        }
    }

    pub(crate) fn try_extra(&self, bytes: u64) -> Result<Option<OptionalWork<'_>>, WsiError> {
        check_cancelled_opt(self.control)?;
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
        // Optional work never jumps an existing FIFO waiter and never waits
        // while holding the operation's ordinary reservation.
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
                holder: None,
            },
            extra: &self.extra,
        }))
    }
}

pub(crate) struct OptionalWork<'a> {
    reservation: TransientReservation,
    extra: &'a Mutex<u64>,
}

impl Drop for OptionalWork<'_> {
    fn drop(&mut self) {
        let mut extra = self.extra.lock().unwrap_or_else(|error| error.into_inner());
        *extra -= self.reservation.bytes;
        // The reservation field releases slide admission after this method.
    }
}
