// Created: 2026-10-02 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Audit port for secret reads and writes (`cpt-cf-credstore-nfr-audit`).
//!
//! The domain service reports every disclosure and every change of a secret
//! through [`AuditSink`] once the operation's outcome is known. The port is
//! deliberately infallible and carries no secret: an implementation owns the
//! best-effort contract (bounded wait, log without the secret, count the
//! failure) so that no audit problem can reach a caller of the service.

use async_trait::async_trait;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// What was done to a secret.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOperation {
    /// A secret was read (point read, `get_secret`, or one item of a collection read selecting `secret`).
    Read,
    /// A record was created carrying a secret.
    Create,
    /// An existing record's secret was replaced (full replace or patch).
    Replace,
    /// An existing record's secret was removed (replace or patch with `null`).
    Remove,
    /// A record holding a secret was deleted.
    Delete,
}

impl AuditOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Create => "create",
            Self::Replace => "replace",
            Self::Remove => "remove",
            Self::Delete => "delete",
        }
    }
}

/// How the audited operation ended.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    Success,
    Failure,
}

impl AuditOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

/// One audit record. Names the subject, the tenant acted in, the reference,
/// the credential type, the operation and its outcome; it has no field that
/// could hold a secret, and no ancestor tenant's id.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub subject_id: Uuid,
    pub tenant_id: Uuid,
    pub reference: String,
    pub secret_type: String,
    pub operation: AuditOperation,
    pub outcome: AuditOutcome,
}

/// Best-effort sink for [`AuditEvent`]s.
///
/// `record` never fails and never panics; an implementation that cannot
/// deliver the event logs an error (without the secret), counts
/// `audit_publish_failed` and returns normally, within a bounded time.
#[async_trait]
pub trait AuditSink: Send + Sync + 'static {
    async fn record(&self, event: AuditEvent);
}

/// Sink that discards every event (service default; tests that do not audit).
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopAudit;

#[async_trait]
impl AuditSink for NoopAudit {
    async fn record(&self, _event: AuditEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_stable() {
        assert_eq!(AuditOperation::Read.as_str(), "read");
        assert_eq!(AuditOperation::Create.as_str(), "create");
        assert_eq!(AuditOperation::Replace.as_str(), "replace");
        assert_eq!(AuditOperation::Remove.as_str(), "remove");
        assert_eq!(AuditOperation::Delete.as_str(), "delete");
        assert_eq!(AuditOutcome::Success.as_str(), "success");
        assert_eq!(AuditOutcome::Failure.as_str(), "failure");
    }

    #[tokio::test]
    async fn noop_sink_accepts_events() {
        NoopAudit
            .record(AuditEvent {
                subject_id: Uuid::nil(),
                tenant_id: Uuid::nil(),
                reference: "r".to_owned(),
                secret_type: "t".to_owned(),
                operation: AuditOperation::Read,
                outcome: AuditOutcome::Success,
            })
            .await;
    }
}
