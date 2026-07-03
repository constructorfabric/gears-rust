// Created: 2026-06-04 by Constructor Tech
//! The [`LockGuard`] handle and its typed command channel back to the backend.
//!
//! A guard is handed to the consumer at acquisition. It carries lease renewal
//! and explicit release back to the backend through a typed command channel
//! with a `oneshot` reply, so the consumer-facing async methods return the
//! backend's *real* result (the same pattern as
//! [`LeaderWatch::resign`](crate::leader::LeaderWatch::resign)):
//!
//! - [`LockGuard::renew`] is **repeatable** (`&self`) — it round-trips a
//!   [`LockRequest::Renew`] and returns the backend's result verbatim, so a
//!   backend-produced [`ClusterError::LockExpired`] surfaces when the TTL has
//!   already elapsed (flow `inst-wt-expired`);
//! - [`LockGuard::release`] **consumes** the guard (`self`) — it round-trips a
//!   [`LockRequest::Release`]; the backend performs the conditional
//!   delete-if-still-holder so a foreign holder cannot release another holder's
//!   lock (`cpt-cf-clst-algo-distributed-lock-release-if-holder`).
//!
//! **Drop is a no-op** — there is intentionally no `Drop` impl. Dropping the
//! guard simply drops the command sender; the backend's
//! [`LockCommandReceiver::recv`] then yields `None` and does nothing, and the
//! lock lapses through TTL expiry — the safety net
//! (`cpt-cf-clst-algo-distributed-lock-ttl-safety`). There are **no fencing
//! tokens**: the no-remote-in-critical-section rule (ADR-002) eliminates the
//! stale-writer scenario fencing tokens would otherwise protect against.

use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::error::{ClusterError, ProviderErrorKind};

/// An internal lock command carrying a one-shot reply channel so the backend
/// can return its real result for an extension or release.
enum LockCommand {
    Renew {
        new_ttl: Duration,
        reply: oneshot::Sender<Result<(), ClusterError>>,
    },
    Release {
        reply: oneshot::Sender<Result<(), ClusterError>>,
    },
}

/// A handle to a held distributed lock (DESIGN §3.1 / §3.3).
///
/// Obtained from [`DistributedLockV1::try_lock`](crate::lock::DistributedLockV1::try_lock)
/// or [`DistributedLockV1::lock`](crate::lock::DistributedLockV1::lock). Refresh
/// the TTL for a longer critical section with [`renew`](Self::renew); release
/// explicitly with [`release`](Self::release).
///
/// **Critical-section rule (ADR-002, DESIGN §2.2/§3.3):** consumers MUST NOT
/// make remote I/O calls inside the critical section between acquisition and
/// [`release`](Self::release). Remote effects MUST happen before acquisition or
/// after release, never between them. This rule — not a fencing token —
/// eliminates the stale-writer scenario. (The lint that enforces it is a
/// separate feature; this guard only documents the rule.)
///
/// **Drop is a no-op** (no I/O in `Drop`): dropping the guard does *not*
/// release — the lock lapses through TTL expiry, the safety net. Use
/// [`release`](Self::release) for immediate release.
#[derive(Debug)]
pub struct LockGuard {
    name: String,
    commands: mpsc::Sender<LockCommand>,
}

impl LockGuard {
    /// Creates a guard and its paired backend-side [`LockCommandReceiver`] for a
    /// held lock `name`.
    ///
    /// `buffer` bounds the in-flight command buffer. A buffer of `1` suffices
    /// when the consumer awaits each [`renew`](Self::renew) before issuing the
    /// next; size it larger only if a guard is shared across tasks that may
    /// issue concurrent renewals.
    ///
    /// # Panics
    /// Panics if `buffer` is zero — a bounded channel requires a non-zero buffer.
    #[must_use]
    pub fn channel(name: String, buffer: usize) -> (LockCommandReceiver, Self) {
        let (tx, rx) = mpsc::channel(buffer);
        let guard = Self { name, commands: tx };
        (LockCommandReceiver { rx }, guard)
    }

    /// The name of the lock this guard holds.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Renews the lock's lease so the critical section has more time.
    /// Repeatable — takes `&self`.
    ///
    /// **Lease semantics (SDK default backend):** the CAS-based default backend
    /// has no remaining-TTL read, so it does **not** add `new_ttl` to the time
    /// already left — it **resets** the lease to `new_ttl` from now. This is why
    /// the method is `renew` (set-the-lease), not `extend` (add-to-the-lease):
    /// pass the *full desired remaining duration*, not an increment. A `new_ttl`
    /// smaller than the lease currently left would **shorten** it, so prefer a
    /// value at least as large as the original acquisition TTL.
    ///
    /// **Window note (SDK default backend):** like [`release`](Self::release),
    /// the renewal is a non-atomic `get`-then-`compare_and_swap` (the cache has
    /// no remaining-TTL read and CAS matches on *version*, not value). It is safe
    /// only while this holder's own TTL is unexpired: if the lease lapses between
    /// the `get` and the CAS and a new holder re-acquires via insert-if-absent
    /// (a fresh entry at version `1`), and this holder's last-seen version was
    /// also `1`, the version-only CAS can match the *foreign* entry and overwrite
    /// it — silently resetting ownership to this holder (strictly worse than the
    /// release window, which only *deletes*). The critical-section rule (ADR-002,
    /// keep the section shorter than the TTL) — or extending the lease before it
    /// lapses — is what keeps this from occurring.
    ///
    /// # Errors
    /// - [`ClusterError::LockExpired`] when the backend reports the TTL had
    ///   already elapsed (the consumer no longer holds the lock and must abort
    ///   the protected operation — flow `inst-wt-expired`), **or** when the
    ///   backend channel is already gone (its task has stopped, so the lock can
    ///   no longer be renewed and necessarily lapses via TTL). The post-shutdown
    ///   best-effort `Ok` narrowing of §3.7 applies only to *release*, never to
    ///   a renewal: a renewal that cannot keep the lock must surface.
    /// - [`ClusterError::Provider`] (`ConnectionLost`) when the backend accepted
    ///   the request but dropped the reply without responding — the renewal is
    ///   unconfirmed, so it propagates rather than being masked (§3.7).
    /// - Any other [`ClusterError`] the backend returns for the renewal.
    pub async fn renew(&self, new_ttl: Duration) -> Result<(), ClusterError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .commands
            .send(LockCommand::Renew {
                new_ttl,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            // Backend's command receiver is gone: its task has stopped, so the
            // claim can no longer be renewed and necessarily lapses via TTL. A
            // renewal cannot keep a lock that nothing maintains — surface it as
            // expired so the consumer aborts (the §3.7 best-effort `Ok` applies
            // to release, not renew).
            return Err(ClusterError::LockExpired {
                name: self.name.clone(),
            });
        }
        match reply_rx.await {
            // The backend completed the renewal and reported its real outcome
            // (`Ok` or `LockExpired` when the TTL had elapsed).
            Ok(result) => result,
            // The backend accepted the request but dropped the responder without
            // replying — a crash / connection loss mid-renewal. The renewal
            // is unconfirmed; §3.7 requires this to propagate, not be masked.
            Err(_) => Err(ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message: "distributed-lock backend dropped the renew \
                          acknowledgement without responding; the TTL renewal \
                          was not confirmed"
                    .to_owned(),
            }),
        }
    }

    /// Releases the lock explicitly. Consumers MUST call this at the end of the
    /// critical section; the TTL only bounds the leak window if they do not.
    ///
    /// Consumes the guard — no further use is possible after releasing. The
    /// backend performs the release conditionally (delete-if-still-holder), so a
    /// foreign holder cannot release another holder's lock
    /// (`cpt-cf-clst-algo-distributed-lock-release-if-holder`).
    ///
    /// **Window note (SDK default backend):** that conditional release is a
    /// non-atomic `get`-then-`delete` (the cache has no CAS delete). It is safe
    /// only while this holder's own TTL is unexpired: keep the critical section
    /// shorter than the lock TTL (ADR-002) — or [`renew`](Self::renew) the
    /// lease before it lapses — so the lease cannot expire, and a new holder
    /// re-acquire, between the check and the delete.
    ///
    /// # Errors
    /// Returns the backend's own result for the release when it replies. Two
    /// teardown cases are distinguished (DESIGN §3.7):
    ///
    /// - **Backend gone** — the request cannot even be delivered (the backend's
    ///   receiver was dropped, e.g. after cluster shutdown). Its task has
    ///   stopped, so the entry can no longer be maintained and lapses via TTL;
    ///   this returns `Ok(())` on a best-effort basis (the post-shutdown
    ///   narrowing).
    /// - **Acknowledgement lost** — the backend accepted the request but dropped
    ///   the reply without responding (a crash or connection loss mid-release).
    ///   The release is *not* confirmed, so this propagates a
    ///   [`ClusterError::Provider`] rather than masking the failure as success;
    ///   the entry still lapses via TTL.
    pub async fn release(self) -> Result<(), ClusterError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .commands
            .send(LockCommand::Release { reply: reply_tx })
            .await
            .is_err()
        {
            // Backend's command receiver is gone, so its task has stopped: the
            // entry can no longer be maintained and necessarily lapses via TTL.
            // Nothing more can be released — best-effort Ok (§3.7).
            return Ok(());
        }
        match reply_rx.await {
            // The backend completed the release and reported its real outcome.
            Ok(result) => result,
            // The backend accepted the request but dropped the responder without
            // replying — it crashed or lost the connection mid-release. §3.7
            // requires this to propagate, not be masked as success: the release
            // is unconfirmed and the entry only lapses via TTL.
            Err(_) => Err(ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message: "distributed-lock backend dropped the release \
                          acknowledgement without responding; the release was \
                          not confirmed and the entry will lapse via TTL"
                    .to_owned(),
            }),
        }
    }
}

/// A consumer command delivered to the backend, paired with a [`LockGuard`] by
/// [`LockGuard::channel`]. The backend's task selects on
/// [`LockCommandReceiver::recv`] and completes each request through its
/// [`LockResponder`].
#[derive(Debug)]
pub enum LockRequest {
    /// Renew the held lock's lease to `new_ttl` from now (the CAS-based default
    /// resets the lease rather than adding to it — see [`LockGuard::renew`]). The
    /// backend replies `Ok(())` on success, or [`ClusterError::LockExpired`] if
    /// the TTL had already elapsed.
    Renew {
        /// The new time-to-live the lease is reset to, measured from now.
        new_ttl: Duration,
        /// The reply side the backend completes with the renewal outcome.
        responder: LockResponder,
    },
    /// Release the held lock. The backend releases conditionally
    /// (delete-if-still-holder) and replies with the outcome.
    Release {
        /// The reply side the backend completes with the release outcome.
        responder: LockResponder,
    },
}

/// The backend-side receiver of consumer [`LockGuard`] commands, paired by
/// [`LockGuard::channel`].
#[derive(Debug)]
pub struct LockCommandReceiver {
    rx: mpsc::Receiver<LockCommand>,
}

impl LockCommandReceiver {
    /// Awaits the next lock command, or `None` once the consumer has dropped the
    /// guard without releasing (the lock then lapses via TTL — the safety net).
    /// Returns a [`LockRequest`] carrying the [`LockResponder`] the backend
    /// completes after performing the operation.
    pub async fn recv(&mut self) -> Option<LockRequest> {
        self.rx.recv().await.map(|command| match command {
            LockCommand::Renew { new_ttl, reply } => LockRequest::Renew {
                new_ttl,
                responder: LockResponder { reply },
            },
            LockCommand::Release { reply } => LockRequest::Release {
                responder: LockResponder { reply },
            },
        })
    }
}

/// The reply side of one lock command. The backend calls [`respond`](Self::respond)
/// with the outcome, which is returned to the consumer from
/// [`LockGuard::renew`] or [`LockGuard::release`].
#[derive(Debug)]
pub struct LockResponder {
    reply: oneshot::Sender<Result<(), ClusterError>>,
}

impl LockResponder {
    /// Completes the command with its outcome. A dropped consumer (no longer
    /// awaiting) is ignored — delivering to a gone receiver is a no-op.
    pub fn respond(self, result: Result<(), ClusterError>) {
        let _outcome = self.reply.send(result);
    }
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod guard_tests;
