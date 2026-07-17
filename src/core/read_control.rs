use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::WsiError;

/// Cloneable cooperative cancellation signal for controlled reads.
#[derive(Debug, Clone, Default)]
pub struct ReadCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl ReadCancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Cooperative controls applied to a tile read without changing legacy APIs.
#[derive(Debug, Clone, Default)]
pub struct ReadControl {
    cancellation: ReadCancellationToken,
}

impl ReadControl {
    #[must_use]
    pub const fn new(cancellation: ReadCancellationToken) -> Self {
        Self { cancellation }
    }

    #[must_use]
    pub fn cancellation(&self) -> &ReadCancellationToken {
        &self.cancellation
    }

    pub(crate) fn check_cancelled(&self) -> Result<(), WsiError> {
        if self.cancellation.is_cancelled() {
            Err(WsiError::Cancelled)
        } else {
            Ok(())
        }
    }
}
