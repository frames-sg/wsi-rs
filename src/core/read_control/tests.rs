use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{
    DicomIndexDiagnostic, DicomIndexMapping, DicomIndexOutcome, ReadCancellationToken, ReadControl,
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

    let diagnostic = DicomIndexDiagnostic {
        outcome: DicomIndexOutcome::BuiltFast {
            mapping: DicomIndexMapping::BasicOffsetTableItems,
        },
        elapsed: Duration::from_millis(7),
    };
    control.record_diagnostic(diagnostic);

    assert_eq!(events.lock().unwrap().as_slice(), &[diagnostic]);
}

#[test]
fn cancellation_controls_recover_a_poisoned_publication_gate() {
    let cancellation = ReadCancellationToken::new();
    let control = ReadControl::new(cancellation.clone());
    assert!(!control.cancellation().is_cancelled());
    assert!(format!("{control:?}").contains("diagnostics_enabled: false"));

    let poisoned_state = Arc::clone(&cancellation.state);
    let _ = std::thread::spawn(move || {
        let _guard = poisoned_state.publication_gate.lock().unwrap();
        panic!("poison publication gate");
    })
    .join();

    cancellation.cancel();
    assert!(control.cancellation().is_cancelled());

    let active = ReadCancellationToken::new();
    let poisoned_state = Arc::clone(&active.state);
    let _ = std::thread::spawn(move || {
        let _guard = poisoned_state.publication_gate.lock().unwrap();
        panic!("poison active publication gate");
    })
    .join();
    let active_control = ReadControl::new(active);
    assert_eq!(active_control.publish_if_active(|| 17).unwrap(), 17);
}

#[test]
fn deferred_diagnostics_recover_a_poisoned_buffer_and_preserve_order() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = ReadControl::default().with_diagnostic_sink(Arc::new(move |event| {
        captured.lock().unwrap().push(event);
    }));
    let (deferred_control, deferred) = control.defer_diagnostics();
    let buffer = Arc::clone(
        deferred
            .diagnostics
            .as_ref()
            .expect("diagnostic buffering is enabled"),
    );
    let _ = std::thread::spawn(move || {
        let _guard = buffer.lock().unwrap();
        panic!("poison deferred diagnostic buffer");
    })
    .join();

    let first = DicomIndexDiagnostic::new(
        DicomIndexOutcome::FastPathFallback,
        Duration::from_millis(2),
    );
    let second =
        DicomIndexDiagnostic::new(DicomIndexOutcome::TokenFallback, Duration::from_millis(3));
    deferred_control.record_diagnostic(first);
    deferred_control.record_diagnostic(second);
    assert!(events.lock().unwrap().is_empty());

    deferred.flush();
    assert_eq!(events.lock().unwrap().as_slice(), &[first, second]);
}
