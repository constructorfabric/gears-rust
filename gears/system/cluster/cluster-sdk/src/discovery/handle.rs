// Created: 2026-06-04 by Constructor Tech
//! The [`ServiceHandle`] registration handle and its typed command channel back
//! to the backend.
//!
//! A handle is returned to the consumer at registration. It carries metadata
//! updates, serving-intent flips, and explicit deregistration back to the
//! backend through a typed command channel with a `oneshot` reply, so each
//! consumer-facing async method returns the backend's *real* result (the same
//! pattern as [`LockGuard`](crate::lock::LockGuard)):
//!
//! - [`ServiceHandle::update_metadata`] and [`ServiceHandle::set_state`] are
//!   **repeatable** (`&self`) — each round-trips its request and returns the
//!   backend's result verbatim; watchers observe a `Change(Updated)`
//!   (`inst-dr-disable`, state transitions `inst-is-disable`/`inst-is-enable`);
//! - [`ServiceHandle::deregister`] **consumes** the handle (`self`) — it
//!   round-trips a deregistration; watchers observe a `Change(Left)`
//!   (`inst-dr-deregister`).
//!
//! **Drop is a no-op** — there is intentionally no `Drop` impl, so dropping the
//! handle performs no I/O. Dropping it simply drops the command sender; the
//! backend's [`ServiceCommandReceiver::recv`] then yields `None` and the
//! instance lapses through its TTL-bounded heartbeat
//! (`cpt-cf-clst-algo-service-discovery-heartbeat`). Use
//! [`deregister`](ServiceHandle::deregister) for immediate removal.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};

use crate::discovery::InstanceState;
use crate::error::{ClusterError, ProviderErrorKind};

/// An internal handle command carrying a one-shot reply channel so the backend
/// can return its real result.
enum ServiceCommand {
    UpdateMetadata {
        metadata: HashMap<String, String>,
        reply: oneshot::Sender<Result<(), ClusterError>>,
    },
    SetState {
        state: InstanceState,
        reply: oneshot::Sender<Result<(), ClusterError>>,
    },
    Deregister {
        reply: oneshot::Sender<Result<(), ClusterError>>,
    },
}

/// A handle to a registered service instance (DESIGN §3.1 / §3.3).
///
/// Obtained from
/// [`ServiceDiscoveryV1::register`](crate::discovery::ServiceDiscoveryV1::register).
/// Update routing attributes with [`update_metadata`](Self::update_metadata),
/// set serving intent with [`set_state`](Self::set_state), and remove the
/// instance explicitly with [`deregister`](Self::deregister).
///
/// **Serving intent, not health (ADR-008):** [`set_state`](Self::set_state)
/// declares whether the module *intends* this instance to take work. It is not a
/// liveness signal — a stuck instance disappears from discovery only when its
/// TTL-bounded heartbeat stops.
///
/// **Drop is a no-op** (no I/O in `Drop`): dropping the handle does *not*
/// deregister — the instance lapses through heartbeat/TTL expiry. Use
/// [`deregister`](Self::deregister) for immediate removal.
#[derive(Debug)]
pub struct ServiceHandle {
    instance_id: String,
    commands: mpsc::Sender<ServiceCommand>,
}

impl ServiceHandle {
    /// Creates a handle and its paired backend-side [`ServiceCommandReceiver`]
    /// for the registered `instance_id`.
    ///
    /// `buffer` bounds the in-flight command buffer. A buffer of `1` suffices
    /// when the consumer awaits each command before issuing the next; size it
    /// larger only if a handle is shared across tasks issuing concurrent
    /// updates.
    ///
    /// # Panics
    /// Panics if `buffer` is zero — a bounded channel requires a non-zero buffer.
    #[must_use]
    pub fn channel(instance_id: String, buffer: usize) -> (ServiceCommandReceiver, Self) {
        let (tx, rx) = mpsc::channel(buffer);
        let handle = Self {
            instance_id,
            commands: tx,
        };
        (ServiceCommandReceiver { rx }, handle)
    }

    /// The instance id this handle registered.
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Replaces the instance's routing metadata. Repeatable — takes `&self`.
    /// Watchers receive a `Change(Updated)`.
    ///
    /// # Errors
    /// - [`ClusterError::Provider`] (`ConnectionLost`) when the backend channel
    ///   is already gone, or when the backend accepted the request but dropped
    ///   the reply without responding — the update is unconfirmed, so it
    ///   surfaces rather than being masked (the §3.7 best-effort `Ok` narrowing
    ///   applies only to *deregister*, never to a metadata update).
    /// - Any other [`ClusterError`] the backend returns for the update.
    pub async fn update_metadata(
        &self,
        metadata: HashMap<String, String>,
    ) -> Result<(), ClusterError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .commands
            .send(ServiceCommand::UpdateMetadata {
                metadata,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return Err(Self::backend_gone_error("metadata update"));
        }
        Self::await_confirmation(reply_rx, "metadata update").await
    }

    /// Sets the instance's module-declared serving intent to `state`
    /// ([`Enabled`](crate::discovery::InstanceState::Enabled) /
    /// [`Disabled`](crate::discovery::InstanceState::Disabled)). Repeatable —
    /// takes `&self`. Watchers receive a `Change(Updated)`.
    ///
    /// NOT a health observation (ADR-008): this declares intent; liveness is
    /// signaled by heartbeat/TTL renewal.
    ///
    /// # Errors
    /// - [`ClusterError::Provider`] (`ConnectionLost`) when the backend channel
    ///   is already gone, or when the backend accepted the request but dropped
    ///   the reply without responding — the transition is unconfirmed and
    ///   surfaces rather than being masked.
    /// - Any other [`ClusterError`] the backend returns for the transition.
    pub async fn set_state(&self, state: InstanceState) -> Result<(), ClusterError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .commands
            .send(ServiceCommand::SetState {
                state,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return Err(Self::backend_gone_error("serving-intent change"));
        }
        Self::await_confirmation(reply_rx, "serving-intent change").await
    }

    /// Deregisters the instance explicitly. Consumes the handle — no further use
    /// is possible. Watchers receive a `Change(Left)`.
    ///
    /// # Errors
    /// Returns the backend's own result for the deregistration when it replies.
    /// Two teardown cases are distinguished (DESIGN §3.7):
    ///
    /// - **Backend gone** — the request cannot even be delivered (the backend's
    ///   receiver was dropped, e.g. after cluster shutdown). Its task has
    ///   stopped, so the instance can no longer be maintained and lapses via the
    ///   heartbeat TTL; this returns `Ok(())` on a best-effort basis (the
    ///   post-shutdown narrowing).
    /// - **Acknowledgement lost** — the backend accepted the request but dropped
    ///   the reply without responding (a crash or connection loss
    ///   mid-deregister). The deregistration is *not* confirmed, so this
    ///   propagates a [`ClusterError::Provider`] rather than masking the failure
    ///   as success; the instance still lapses via TTL.
    pub async fn deregister(self) -> Result<(), ClusterError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .commands
            .send(ServiceCommand::Deregister { reply: reply_tx })
            .await
            .is_err()
        {
            // Backend's command receiver is gone, so its task has stopped: the
            // instance can no longer be maintained and necessarily lapses via
            // the heartbeat TTL. Nothing more can be removed — best-effort Ok
            // (§3.7 post-shutdown narrowing).
            return Ok(());
        }
        match reply_rx.await {
            // The backend completed the deregistration and reported its outcome.
            Ok(result) => result,
            // The backend accepted the request but dropped the responder without
            // replying — a crash / connection loss mid-deregister. §3.7 requires
            // this to propagate, not be masked as success: the removal is
            // unconfirmed and the instance only lapses via TTL.
            Err(_) => Err(ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message: "service-discovery backend dropped the deregister \
                          acknowledgement without responding; the deregistration \
                          was not confirmed and the instance will lapse via TTL"
                    .to_owned(),
            }),
        }
    }

    /// The error a repeatable mutation returns when the backend command channel
    /// is gone: the request could not be delivered, so the mutation is
    /// unconfirmed. Unlike *deregister*, a repeatable update must surface this
    /// rather than narrow it to `Ok` (§3.7).
    fn backend_gone_error(operation: &str) -> ClusterError {
        ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            message: format!(
                "service-discovery backend channel is gone; the {operation} was not delivered"
            ),
        }
    }

    /// Awaits the backend's reply for a repeatable mutation, mapping a dropped
    /// responder to an unconfirmed-mutation error.
    async fn await_confirmation(
        reply_rx: oneshot::Receiver<Result<(), ClusterError>>,
        operation: &str,
    ) -> Result<(), ClusterError> {
        match reply_rx.await {
            Ok(result) => result,
            Err(_) => Err(ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message: format!(
                    "service-discovery backend dropped the {operation} acknowledgement \
                     without responding; it was not confirmed"
                ),
            }),
        }
    }
}

/// A consumer command delivered to the backend, paired with a [`ServiceHandle`]
/// by [`ServiceHandle::channel`]. The backend's task selects on
/// [`ServiceCommandReceiver::recv`] and completes each request through its
/// [`ServiceResponder`].
#[derive(Debug)]
pub enum ServiceRequest {
    /// Replace the instance's routing metadata. The backend applies it and
    /// replies with the outcome; watchers observe a `Change(Updated)`.
    UpdateMetadata {
        /// The replacement metadata map.
        metadata: HashMap<String, String>,
        /// The reply side the backend completes with the update outcome.
        responder: ServiceResponder,
    },
    /// Set the instance's serving intent. The backend applies it and replies;
    /// watchers observe a `Change(Updated)`.
    SetState {
        /// The requested serving intent.
        state: InstanceState,
        /// The reply side the backend completes with the transition outcome.
        responder: ServiceResponder,
    },
    /// Deregister the instance. The backend removes it and replies; watchers
    /// observe a `Change(Left)`.
    Deregister {
        /// The reply side the backend completes with the deregistration outcome.
        responder: ServiceResponder,
    },
}

/// The backend-side receiver of consumer [`ServiceHandle`] commands, paired by
/// [`ServiceHandle::channel`].
#[derive(Debug)]
pub struct ServiceCommandReceiver {
    rx: mpsc::Receiver<ServiceCommand>,
}

impl ServiceCommandReceiver {
    /// Awaits the next handle command, or `None` once the consumer has dropped
    /// the handle without deregistering (the instance then lapses via the
    /// heartbeat TTL). Returns a [`ServiceRequest`] carrying the
    /// [`ServiceResponder`] the backend completes after performing the
    /// operation.
    pub async fn recv(&mut self) -> Option<ServiceRequest> {
        self.rx.recv().await.map(|command| match command {
            ServiceCommand::UpdateMetadata { metadata, reply } => ServiceRequest::UpdateMetadata {
                metadata,
                responder: ServiceResponder { reply },
            },
            ServiceCommand::SetState { state, reply } => ServiceRequest::SetState {
                state,
                responder: ServiceResponder { reply },
            },
            ServiceCommand::Deregister { reply } => ServiceRequest::Deregister {
                responder: ServiceResponder { reply },
            },
        })
    }
}

/// The reply side of one handle command. The backend calls
/// [`respond`](Self::respond) with the outcome, which is returned to the
/// consumer from the originating [`ServiceHandle`] method.
#[derive(Debug)]
pub struct ServiceResponder {
    reply: oneshot::Sender<Result<(), ClusterError>>,
}

impl ServiceResponder {
    /// Completes the command with its outcome. A dropped consumer (no longer
    /// awaiting) is ignored — delivering to a gone receiver is a no-op.
    pub fn respond(self, result: Result<(), ClusterError>) {
        let _outcome = self.reply.send(result);
    }
}

#[cfg(test)]
#[path = "handle_tests.rs"]
mod handle_tests;
