//! Background self-registration (directory presence) for `OoP` gears
//! (`cpt-cf-component-oop-bootstrap`).
//!
//! [`presence_loop`] is the directory **writer** for an `OoP` instance: it
//! registers the instance's gRPC/REST endpoints and `OpenAPI` spec (retrying
//! with exponential backoff, 100ms → 30s cap) and re-registers whenever the
//! `OpenAPI` document changes. It sends no heartbeats itself — the per-process
//! [`PresenceManager`](super::PresenceManager) owns lease renewal and
//! `NOT_FOUND` re-registration for every record it writes.
//!
//! Dependency resolution is **not** here: consumers are wired by the
//! proxy-wiring phase via typed `#[toolkit::consumes]` directory-resolving
//! clients, which feed the shared
//! [`DependencyChecker`](super::readiness::DependencyChecker) that gates
//! `/readyz`.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use cf_system_sdks::directory::{
    DirectoryInvalidArgument, DirectoryPermissionDenied, RegisterInstanceInfo,
};

use crate::runtime::PresenceManager;

/// Initial retry backoff for registration and dependency polling.
const INITIAL_BACKOFF: Duration = Duration::from_millis(100);
/// Maximum retry backoff (cap).
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Next backoff in the exponential schedule (doubles, capped at [`MAX_BACKOFF`]).
fn next_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_BACKOFF)
}

/// Sleep for `dur`, returning early (`false`) if `cancel` fires first.
async fn sleep_or_cancel(dur: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(dur) => true,
    }
}

/// Classification of a failed registration attempt.
///
/// Permanence drives the retry decision and is *independent* of any log text, so
/// a permanent rejection with no remediation hint still stops the loop rather
/// than being retried forever.
enum RejectionKind {
    /// Retrying the identical request can never succeed — stop the backoff and
    /// log loudly. The optional string is an operator-facing "what to check"
    /// hint (the rejection *category* is already in the sentinel's `Display`).
    Permanent(Option<&'static str>),
    /// Transient or unrecognised — keep retrying with backoff.
    Transient,
}

/// Classify a registration error for the retry decision.
fn classify_rejection(err: &anyhow::Error) -> RejectionKind {
    if err.is::<DirectoryInvalidArgument>() {
        return RejectionKind::Permanent(Some("Check its configured labels and endpoint URIs"));
    }
    if err.is::<DirectoryPermissionDenied>() {
        return RejectionKind::Permanent(Some(
            "Check that this peer is authorized to register this gear (its ServiceAccount \
             namespace / trust domain allowlist)",
        ));
    }
    RejectionKind::Transient
}

/// Register `info` with the directory, retrying with exponential backoff.
///
/// Returns `true` if the presence loop should keep running (the directory
/// accepted the registration, or [permanently rejected](classify_rejection)
/// it), and `false` only if the task was cancelled. A permanent rejection still
/// returns `true`, so the caller keeps the presence loop alive; see
/// [`presence_loop`].
///
/// **Not cancel-safe under `abort()`:** `cancel` is checked only between attempts,
/// so the token exits the loop cleanly between RPCs but an abort can drop this
/// mid-`register_instance`. Shutdown aborts the task then deregisters, so an abort
/// landing mid-RPC can leave a registration committed after the deregister — a
/// stale instance until its heartbeat TTL evicts it.
async fn register_once_with_backoff(
    presence: &Arc<PresenceManager>,
    info: &RegisterInstanceInfo,
    cancel: &CancellationToken,
) -> bool {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        if cancel.is_cancelled() {
            return false;
        }
        match presence.register(info.clone()).await {
            Ok(()) => {
                tracing::info!(gear = %info.gear, instance = %info.instance_id, "registered with DirectoryService");
                return true;
            }
            Err(e) => {
                if let RejectionKind::Permanent(guidance) = classify_rejection(&e) {
                    tracing::error!(
                        gear = %info.gear,
                        instance = %info.instance_id,
                        error = %e,
                        guidance = guidance.unwrap_or("(no remediation hint available)"),
                        "registration permanently rejected by the directory; this instance \
                         may not be discoverable"
                    );
                    return true;
                }
                tracing::warn!(
                    gear = %info.gear,
                    error = %e,
                    backoff_ms = backoff.as_millis(),
                    "registration attempt failed; retrying"
                );
                if !sleep_or_cancel(backoff, cancel).await {
                    return false;
                }
                backoff = next_backoff(backoff);
            }
        }
    }
}

/// Directory **writer** for an `OoP` instance.
///
/// Registers `info` (with backoff) through the per-process [`PresenceManager`],
/// which then owns the record's lease renewal, `NOT_FOUND` re-registration, the
/// `Draining` drain signal and deregistration. This writer itself only
/// (re-)registers the record; it sends no heartbeats. It returns once `info` is
/// registered (or permanently rejected) and waits for `cancel`.
///
/// A [permanent rejection](classify_rejection) (a malformed request or an
/// authorization denial) is logged loudly but does not stop presence: the
/// record stays tracked by the manager, which keeps renewing any registration
/// that did land. A *recoverable* rejection (a gRPC service-name conflict that
/// clears once the current owner deregisters) is retried with backoff.
pub(super) async fn presence_loop(
    presence: Arc<PresenceManager>,
    info: RegisterInstanceInfo,
    cancel: CancellationToken,
) {
    let (_no_updates, updates) = tokio::sync::watch::channel(info);
    presence_loop_with_updates(presence, updates, cancel).await;
}

/// [`presence_loop`] with replaceable registration info (e.g. an updated
/// `OpenAPI` document): each update re-registers through the manager so the
/// renewed record carries the new document. Renewal itself is the manager's job.
pub(super) async fn presence_loop_with_updates(
    presence: Arc<PresenceManager>,
    mut updates: tokio::sync::watch::Receiver<RegisterInstanceInfo>,
    cancel: CancellationToken,
) {
    let mut info = updates.borrow_and_update().clone();
    if !register_once_with_backoff(&presence, &info, &cancel).await {
        return;
    }

    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            changed = updates.changed() => {
                if changed.is_err() {
                    // No more updates: the manager keeps renewing the current
                    // record until it is cancelled/deregistered.
                    cancel.cancelled().await;
                    return;
                }
                info = updates.borrow_and_update().clone();
                // Re-register with the new document; the manager picks up the
                // updated info for subsequent renewals.
                if !register_once_with_backoff(&presence, &info, &cancel).await {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "oop_registration_tests.rs"]
mod tests;
