//! Webhook emitter trait used by lifecycle services.
//!
//! Phase 4 declares only the dependency-injection point — the full webhook
//! delivery pipeline lives in Phase 14 (REST surface assembly + outbox
//! wiring). Services that need to broadcast a lifecycle transition take an
//! `Arc<dyn WebhookEmitter>` so the production wiring can swap in the real
//! transactional implementation later without changing call sites.
//!
//! The default [`NoopWebhookEmitter`] swallows events with a `debug!` trace
//! — appropriate for tests, scaffolding, and the bring-up of downstream
//! phases that don't yet require external delivery.
//
// @cpt-cf-chat-engine-webhook-emitter:p4

use async_trait::async_trait;
use modkit_macros::domain_model;
use tracing::debug;
use uuid::Uuid;

use crate::domain::error::Result;

/// Lifecycle events that Chat Engine emits to webhook subscribers.
#[domain_model]
#[derive(Debug, Clone)]
pub enum WebhookEvent {
    /// `POST /sessions` succeeded — the plugin has accepted the new session
    /// and `enabled_capabilities` has been persisted.
    SessionCreated {
        session_id: Uuid,
        tenant_id: String,
        user_id: String,
        session_type_id: Option<Uuid>,
    },
    /// `POST /sessions/{id}/archive` succeeded.
    SessionArchived {
        session_id: Uuid,
        tenant_id: String,
        user_id: String,
    },
    /// `POST /sessions/{id}/restore` succeeded — fires for both
    /// `archived → active` and `soft_deleted → active`.
    SessionRestored {
        session_id: Uuid,
        tenant_id: String,
        user_id: String,
    },
    /// `DELETE /sessions/{id}` (soft path) succeeded.
    SessionSoftDeleted {
        session_id: Uuid,
        tenant_id: String,
        user_id: String,
    },
    /// `DELETE /sessions/{id}?hard=true` succeeded.
    SessionHardDeleted {
        session_id: Uuid,
        tenant_id: String,
        user_id: String,
    },
    /// `DELETE /sessions/{session_id}/messages/{message_id}` (Phase 12)
    /// succeeded. `deleted_count` is the total number of message rows
    /// removed in the cascade (the target message plus every descendant).
    /// `deleted_at` is the UTC RFC-3339 timestamp captured immediately
    /// after the SERIALIZABLE transaction committed.
    MessageDeleted {
        session_id: Uuid,
        message_id: Uuid,
        tenant_id: String,
        user_id: String,
        deleted_count: u64,
        deleted_at: time::OffsetDateTime,
    },
}

impl WebhookEvent {
    /// Stable string discriminator used for logging and metrics labels.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SessionCreated { .. } => "session.created",
            Self::SessionArchived { .. } => "session.archived",
            Self::SessionRestored { .. } => "session.restored",
            Self::SessionSoftDeleted { .. } => "session.soft_deleted",
            Self::SessionHardDeleted { .. } => "session.hard_deleted",
            Self::MessageDeleted { .. } => "message.deleted",
        }
    }
}

/// Sink for lifecycle webhook events.
///
/// Services hold `Arc<dyn WebhookEmitter>` and call `emit` after every
/// successful lifecycle transition. The trait method is `async` so the
/// production implementation can journal events into a transactional outbox;
/// callers MUST treat the failure of `emit` as non-fatal (webhook delivery
/// is an at-least-once guarantee that is decoupled from the lifecycle write
/// itself).
#[async_trait]
pub trait WebhookEmitter: Send + Sync {
    /// Enqueue the given event for delivery. Errors are advisory: the caller
    /// has already committed the lifecycle change.
    async fn emit(&self, event: WebhookEvent) -> Result<()>;
}

/// Default no-op emitter used during bring-up and unit tests.
#[domain_model]
#[derive(Debug, Default, Clone)]
pub struct NoopWebhookEmitter;

#[async_trait]
impl WebhookEmitter for NoopWebhookEmitter {
    async fn emit(&self, event: WebhookEvent) -> Result<()> {
        debug!(
            event = event.kind(),
            "webhook emitter (noop) — event swallowed"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_returns_ok() {
        let emitter = NoopWebhookEmitter;
        emitter
            .emit(WebhookEvent::SessionArchived {
                session_id: Uuid::nil(),
                tenant_id: "t".into(),
                user_id: "u".into(),
            })
            .await
            .expect("noop emitter always succeeds");
    }

    #[test]
    fn event_kind_strings_are_stable() {
        let cases = [
            (
                WebhookEvent::SessionCreated {
                    session_id: Uuid::nil(),
                    tenant_id: "t".into(),
                    user_id: "u".into(),
                    session_type_id: None,
                },
                "session.created",
            ),
            (
                WebhookEvent::SessionArchived {
                    session_id: Uuid::nil(),
                    tenant_id: "t".into(),
                    user_id: "u".into(),
                },
                "session.archived",
            ),
            (
                WebhookEvent::SessionRestored {
                    session_id: Uuid::nil(),
                    tenant_id: "t".into(),
                    user_id: "u".into(),
                },
                "session.restored",
            ),
            (
                WebhookEvent::SessionSoftDeleted {
                    session_id: Uuid::nil(),
                    tenant_id: "t".into(),
                    user_id: "u".into(),
                },
                "session.soft_deleted",
            ),
            (
                WebhookEvent::SessionHardDeleted {
                    session_id: Uuid::nil(),
                    tenant_id: "t".into(),
                    user_id: "u".into(),
                },
                "session.hard_deleted",
            ),
            (
                WebhookEvent::MessageDeleted {
                    session_id: Uuid::nil(),
                    message_id: Uuid::nil(),
                    tenant_id: "t".into(),
                    user_id: "u".into(),
                    deleted_count: 1,
                    deleted_at: time::OffsetDateTime::UNIX_EPOCH,
                },
                "message.deleted",
            ),
        ];
        for (evt, expected) in cases {
            assert_eq!(evt.kind(), expected);
        }
    }
}
