use super::*;

/// The regression this function exists for: a handle dropped without `stop()`
/// must have cancelled the shared token *before* the caller reaches its
/// diagnosis, so the background tasks unwind. The Postgres plugin's combined
/// handle shipped without this and leaked both reapers and both `LISTEN` tasks —
/// each pinning a pool clone, so the pool never closed either — for the life of
/// the process.
#[test]
fn dropping_without_stop_cancels_before_diagnosing() {
    let shutdown = CancellationToken::new();

    let diagnosis = cancel_and_diagnose_drop(false, &shutdown);

    assert_eq!(
        diagnosis,
        DropDiagnosis::Unstopped,
        "a handle dropped without stop() outside a panic must be reported"
    );
    assert!(
        shutdown.is_cancelled(),
        "the token must already be cancelled by the time the caller diagnoses, or the debug-build \
         panic makes the cancel unreachable"
    );
}

/// A clean `stop()` has already cancelled the token itself, and the handle
/// carries nothing left to unwind — so `Drop` must stay silent rather than
/// re-reporting a shutdown that worked.
#[test]
fn a_cleanly_stopped_handle_is_not_diagnosed() {
    let shutdown = CancellationToken::new();

    assert_eq!(
        cancel_and_diagnose_drop(true, &shutdown),
        DropDiagnosis::StoppedCleanly
    );
}

/// `stopped == true` must not cancel on its own account. The flag means `stop()`
/// completed, which already cancelled the token; cancelling here would be
/// harmless but would make this function's contract ("cancels unless the handle
/// stopped cleanly") false, and the next reader would have to re-derive which of
/// the two actually holds.
#[test]
fn a_cleanly_stopped_handle_does_not_cancel_here() {
    let shutdown = CancellationToken::new();

    let _diagnosis = cancel_and_diagnose_drop(true, &shutdown);

    assert!(
        !shutdown.is_cancelled(),
        "stop() owns the cancel on the clean path; Drop must not claim it too"
    );
}

/// Cancelling is unconditional, but the *diagnosis* is not: during a panic
/// unwind the caller must warn instead of panicking, since a second panic
/// aborts. The token still has to be cancelled — a test failing mid-critical
/// section is precisely when the background tasks most need to be told.
#[test]
fn dropping_during_a_panic_still_cancels_but_asks_for_a_warning() {
    // `std::thread::panicking()` is only true inside an unwinding drop, so drive
    // the real thing rather than faking the predicate.
    struct Dropper {
        shutdown: CancellationToken,
        diagnosis: std::sync::Arc<std::sync::Mutex<Option<DropDiagnosis>>>,
    }
    impl Drop for Dropper {
        fn drop(&mut self) {
            let diagnosis = cancel_and_diagnose_drop(false, &self.shutdown);
            *self.diagnosis.lock().expect("uncontended") = Some(diagnosis);
        }
    }

    let shutdown = CancellationToken::new();
    let diagnosis = std::sync::Arc::new(std::sync::Mutex::new(None));
    let panicked = std::panic::catch_unwind({
        let shutdown = shutdown.clone();
        let diagnosis = std::sync::Arc::clone(&diagnosis);
        move || {
            let _dropper = Dropper {
                shutdown,
                diagnosis,
            };
            panic!(
                "simulated failure mid-shutdown (expected: this test drives a real unwinding \
                 Drop, so this line on stderr is part of a passing run)"
            );
        }
    });

    assert!(panicked.is_err(), "setup: the closure must have panicked");
    assert_eq!(
        *diagnosis.lock().expect("uncontended"),
        Some(DropDiagnosis::DuringPanic),
        "a Drop running during unwind must ask for a warning, never a second panic"
    );
    assert!(
        shutdown.is_cancelled(),
        "cancelling is unconditional: a panicking shutdown still has to unwind its tasks"
    );
}
