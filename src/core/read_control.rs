use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::WsiError;

/// Mapping selected by the validated fast DICOM encapsulated-frame indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DicomIndexMapping {
    /// Extended offsets directly described one Item per frame.
    ExtendedOffsetTableDirect,
    /// Extended offsets were resolved after scanning Item headers.
    ExtendedOffsetTableItems,
    /// Basic offsets were resolved after scanning Item headers.
    BasicOffsetTableItems,
    /// A single frame owns every scanned fragment.
    SingleFrameItems,
    /// An empty table was safely resolved as one fragment per frame.
    OneFragmentPerFrame,
}

/// Outcome of one DICOM encapsulated-frame index operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DicomIndexOutcome {
    /// The validated seek-based indexer completed successfully.
    BuiltFast { mapping: DicomIndexMapping },
    /// The seek-based path could not safely handle the input and yielded to
    /// the token parser.
    FastPathFallback,
    /// The token parser completed the index after a fast-path fallback.
    TokenFallback,
    /// A previously completed index was reused.
    Reused,
}

/// Typed timing emitted for one DICOM encapsulated-frame index operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DicomIndexDiagnostic {
    pub outcome: DicomIndexOutcome,
    pub elapsed: Duration,
}

impl DicomIndexDiagnostic {
    #[must_use]
    pub const fn new(outcome: DicomIndexOutcome, elapsed: Duration) -> Self {
        Self { outcome, elapsed }
    }
}

/// Optional diagnostics emitted by cancellation-aware reads and preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadDiagnostic {
    DicomIndex(DicomIndexDiagnostic),
}

/// Receives opt-in controlled-read diagnostics.
pub trait ReadDiagnosticSink: Send + Sync {
    fn record(&self, diagnostic: ReadDiagnostic);
}

impl<F> ReadDiagnosticSink for F
where
    F: Fn(ReadDiagnostic) + Send + Sync,
{
    fn record(&self, diagnostic: ReadDiagnostic) {
        self(diagnostic);
    }
}

/// Cloneable cooperative cancellation signal for controlled reads.
#[derive(Debug, Default)]
struct ReadCancellationState {
    cancelled: AtomicBool,
    publication_gate: Mutex<()>,
}

#[derive(Debug, Clone, Default)]
pub struct ReadCancellationToken {
    state: Arc<ReadCancellationState>,
}

impl ReadCancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        let _publication = self
            .state
            .publication_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.state.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }
}

/// Cooperative controls applied to a tile read without changing legacy APIs.
#[derive(Clone, Default)]
pub struct ReadControl {
    cancellation: ReadCancellationToken,
    diagnostic_sink: Option<Arc<dyn ReadDiagnosticSink>>,
}

pub(crate) struct DeferredReadDiagnostics {
    sink: Option<Arc<dyn ReadDiagnosticSink>>,
    diagnostics: Option<Arc<Mutex<Vec<ReadDiagnostic>>>>,
}

impl DeferredReadDiagnostics {
    pub(crate) fn flush(self) {
        let Some(buffered) = self.diagnostics else {
            return;
        };
        let diagnostics = {
            let mut guard = buffered.lock().unwrap_or_else(|error| error.into_inner());
            std::mem::take(&mut *guard)
        };
        if let Some(sink) = self.sink {
            for diagnostic in diagnostics {
                sink.record(diagnostic);
            }
        }
    }
}

impl std::fmt::Debug for ReadControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadControl")
            .field("cancellation", &self.cancellation)
            .field("diagnostics_enabled", &self.diagnostic_sink.is_some())
            .finish()
    }
}

impl ReadControl {
    #[must_use]
    pub const fn new(cancellation: ReadCancellationToken) -> Self {
        Self {
            cancellation,
            diagnostic_sink: None,
        }
    }

    /// Attaches an opt-in diagnostic sink while preserving the cancellation
    /// token and controlled-read behavior.
    #[must_use]
    pub fn with_diagnostic_sink(mut self, sink: Arc<dyn ReadDiagnosticSink>) -> Self {
        self.diagnostic_sink = Some(sink);
        self
    }

    #[must_use]
    pub fn cancellation(&self) -> &ReadCancellationToken {
        &self.cancellation
    }

    /// Returns whether this control has an attached diagnostic sink.
    #[must_use]
    pub fn diagnostics_enabled(&self) -> bool {
        self.diagnostic_sink.is_some()
    }

    /// Records one diagnostic when a sink is attached. This is a no-op for
    /// the default control.
    pub fn record_diagnostic(&self, diagnostic: ReadDiagnostic) {
        if let Some(sink) = &self.diagnostic_sink {
            sink.record(diagnostic);
        }
    }

    pub(crate) fn defer_diagnostics(&self) -> (Self, DeferredReadDiagnostics) {
        let Some(sink) = self.diagnostic_sink.as_ref() else {
            return (
                self.clone(),
                DeferredReadDiagnostics {
                    sink: None,
                    diagnostics: None,
                },
            );
        };
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let diagnostic_sink = {
            let diagnostics = Arc::clone(&diagnostics);
            Some(Arc::new(move |diagnostic: ReadDiagnostic| {
                diagnostics
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(diagnostic);
            }) as Arc<dyn ReadDiagnosticSink>)
        };
        let deferred = DeferredReadDiagnostics {
            sink: Some(Arc::clone(sink)),
            diagnostics: Some(diagnostics),
        };
        (
            Self {
                cancellation: self.cancellation.clone(),
                diagnostic_sink,
            },
            deferred,
        )
    }

    /// Returns [`WsiError::Cancelled`] when cancellation has been requested.
    ///
    /// Format-specific readers can call this at safe cooperative boundaries
    /// without changing the legacy read APIs.
    pub fn check_cancelled(&self) -> Result<(), WsiError> {
        if self.cancellation.is_cancelled() {
            Err(WsiError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn publish_if_active<T>(&self, publish: impl FnOnce() -> T) -> Result<T, WsiError> {
        let _publication = self
            .cancellation
            .state
            .publication_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.check_cancelled()?;
        Ok(publish())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        DicomIndexDiagnostic, DicomIndexMapping, DicomIndexOutcome, ReadControl, ReadDiagnostic,
    };

    #[test]
    fn diagnostic_sink_is_opt_in_and_receives_typed_index_events() {
        let disabled = ReadControl::default();
        assert!(!disabled.diagnostics_enabled());

        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let control = ReadControl::default().with_diagnostic_sink(Arc::new(move |event| {
            captured.lock().unwrap().push(event);
        }));
        assert!(control.diagnostics_enabled());

        let diagnostic = ReadDiagnostic::DicomIndex(DicomIndexDiagnostic {
            outcome: DicomIndexOutcome::BuiltFast {
                mapping: DicomIndexMapping::BasicOffsetTableItems,
            },
            elapsed: Duration::from_millis(7),
        });
        control.record_diagnostic(diagnostic);

        assert_eq!(events.lock().unwrap().as_slice(), &[diagnostic]);
    }
}
