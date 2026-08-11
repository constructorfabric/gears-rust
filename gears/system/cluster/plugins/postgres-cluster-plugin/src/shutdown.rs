//! Shutdown machinery shared by both plugin handles — [`PostgresClusterHandle`]
//! (cache + lock) and [`PostgresLockHandle`] (lock only) — DESIGN.md §10.
//!
//! Both handles end the same way: cancel the shared `CancellationToken`, join
//! the background tasks, hand back this instance's locks, close the beacon, then
//! close the pool. The two steps that are *not* obvious live here rather than in
//! either handle, because each one is a property of the pair and the pair had
//! already drifted on both:
//!
//! * [`close_pool`] bounds the final `PgPool::close()`. `PostgresLockHandle`
//!   used it; `PostgresClusterHandle` called `pool.close()` directly, so the
//!   bound DESIGN.md §10 and §11 both describe existed in the standalone plugin
//!   and not in the combined one that actually ships.
//! * [`cancel_and_diagnose_drop`] is the `Drop` backstop for a `stop()` that
//!   never completed. `PostgresLockHandle::drop` cancelled the token first;
//!   `PostgresClusterHandle::drop` did not, so a dropped `stop()` future leaked
//!   every background task and the pool with them — while DESIGN.md §11 asserted
//!   that "both handle `Drop`s also cancel the shared token".
//!
//! One implementation each, so neither can drift again: the shape of the bug in
//! both cases was two copies of one rule.

use std::time::Duration;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Upper bound on the final `PgPool::close()` in either handle's `stop()`
/// (DESIGN.md §10).
///
/// `close()` waits until **every** checked-out connection has been returned, and
/// not every pool user is something `stop()` joins. The per-lock guard tasks are
/// spawned detached, so one parked in the middle of a `renew`'s or `release()`'s
/// pool I/O is neither cancelled by the shutdown token (a `select!` cannot preempt
/// a branch body already running) nor awaited anywhere — and against an
/// unresponsive backend `sqlx` applies no read timeout of its own. Unbounded, that
/// relocated the shutdown stall out of the joins DESIGN.md §11 tells operators to
/// budget for and into a step with no budget at all. Cache operations share the
/// same pool and the same exposure, which is the other reason this belongs here
/// rather than under `lock/`.
///
/// Bounding it is safe because `close()` marks the pool closed *before* it starts
/// waiting: giving up on the wait still leaves the pool closed, idle connections
/// dropped, and any straggler connection closed when its holder returns it. What
/// is lost is only the guarantee that every backend is gone by the time `stop()`
/// returns — which was never a guarantee an unresponsive peer could keep anyway.
///
/// Ten seconds: long enough that any healthy in-flight statement finishes well
/// inside it (the pool's own `pool_acquire_timeout_ms` default is 5s), short
/// enough to stay inside a typical supervisor's shutdown budget.
pub const POOL_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// [`PgPool::close`] under [`POOL_CLOSE_TIMEOUT`], logging if the budget elapses
/// rather than blocking `stop()` indefinitely.
///
/// The signal keeps its historical `cluster.lock.pool_close_timeout` name even
/// though the pool it closes carries cache traffic too — DESIGN.md §11 names it,
/// and renaming a signal is a contract change rather than a cleanup.
pub async fn close_pool(pool: &PgPool) {
    if tokio::time::timeout(POOL_CLOSE_TIMEOUT, pool.close())
        .await
        .is_err()
    {
        warn!(
            timeout_ms = u64::try_from(POOL_CLOSE_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            "cluster.lock.pool_close_timeout: the write pool still had connections checked out at \
             shutdown; the pool is closed and they are closed on return, but at least one backend \
             outlives stop()"
        );
    }
}

/// What a handle's `Drop` should say about itself, once
/// [`cancel_and_diagnose_drop`] has done the part that must happen regardless.
#[derive(Debug, PartialEq, Eq)]
pub enum DropDiagnosis {
    /// `stop()` ran to completion. Nothing to cancel and nothing to report.
    StoppedCleanly,
    /// Dropped while a panic was already unwinding. The caller should warn and
    /// **not** panic: a second panic during unwind aborts the process, which
    /// would replace a legible test failure with a bare `SIGABRT`.
    DuringPanic,
    /// Dropped without `stop()` outside a panic — the case worth shouting about
    /// (panic in debug, warn in release, per ADR-006 §Confirmation).
    Unstopped,
}

/// Cancels `shutdown` unless the handle stopped cleanly, then reports what the
/// caller should do about it.
///
/// **The cancel comes first and unconditionally, and that ordering is the whole
/// point of this function.** Reaching a handle's `Drop` with `stopped == false`
/// covers two cases, and the second is the one that matters operationally:
///
/// 1. a handle genuinely forgotten — the programming error the diagnosis exists
///    to shout about; and
/// 2. a `stop()` whose *future was dropped part-way*, which is exactly what
///    `tokio::time::timeout(D, handle.stop())` does when its budget elapses —
///    the supervisor-level pattern DESIGN.md §11 recommends.
///
/// In both, cancelling is what lets the reapers and the LISTEN tasks observe
/// shutdown and exit instead of running forever against a handle nobody owns,
/// each still holding a `PgPool` clone that consequently never closes. In case 2
/// the handle is not misused at all, so a diagnosis that ran *instead of*
/// cancelling would punish the caller for following the documented advice.
///
/// It must also precede the caller's `#[cfg(debug_assertions)] panic!`, or the
/// cancel would be unreachable in debug builds — which is why this returns a
/// [`DropDiagnosis`] rather than emitting the diagnosis itself. The panic is
/// compiled in exactly the configuration tests run in, so "the token was
/// cancelled on the path that would otherwise panic" is not observable from a
/// test that lets the `Drop` impl run to completion. Returning the decision
/// makes that ordering directly assertable (see this module's tests), and the
/// end-to-end observable — a cancelled token makes the next `try_lock` answer
/// `ClusterError::Shutdown` — is asserted by an integration scenario gated on
/// `not(debug_assertions)`.
///
/// The beacon is deliberately *not* on this token (`lock::beacon`): it has to
/// outlive the shared cancel so the shutdown drain can still read its key.
/// `BeaconHandle`'s own `Drop` ends that one.
pub fn cancel_and_diagnose_drop(stopped: bool, shutdown: &CancellationToken) -> DropDiagnosis {
    if stopped {
        return DropDiagnosis::StoppedCleanly;
    }
    shutdown.cancel();
    if std::thread::panicking() {
        return DropDiagnosis::DuringPanic;
    }
    DropDiagnosis::Unstopped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this function exists for: a handle dropped without
    /// `stop()` must have cancelled the shared token *before* the caller reaches
    /// its diagnosis, so the background tasks unwind. `PostgresClusterHandle`
    /// shipped without this and leaked its two reapers and two LISTEN tasks —
    /// each holding a `PgPool` clone — for the life of the process.
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
            "the token must already be cancelled by the time the caller diagnoses, or the \
             debug-build panic makes the cancel unreachable"
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

    /// `stopped == true` must not cancel on its own account. The flag means
    /// `stop()` completed, which already cancelled the token; cancelling here
    /// would be harmless but would make this function's contract ("cancels
    /// unless the handle stopped cleanly") false, and the next reader would have
    /// to re-derive which of the two actually holds.
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
        // `std::thread::panicking()` is only true inside an unwinding drop, so
        // drive the real thing rather than faking the predicate.
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
}
