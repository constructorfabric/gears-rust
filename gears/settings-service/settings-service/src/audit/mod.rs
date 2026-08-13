// Created: 2026-08-13 by Constructor Tech
//! The shared Audit Emitter.
//!
//! Every mutating feature publishes through here rather than writing its own
//! records, so the actor, the target, the before and after state, and the
//! resource id are captured the same way regardless of which feature performed
//! the mutation.
//!
//! # Audit is a show-stopper, not telemetry
//!
//! DESIGN.md §4.2: audit is **always active** and the write is **synchronous
//! and fail-closed**. [`AuditEmitter::audit`] therefore returns a `Result` that
//! callers must propagate — a mutation whose audit record could not be written
//! must not be reported as having succeeded, because the trail is the only
//! record that it happened at all.
//!
//! # Pre- and post-image
//!
//! [`AuditRecord`] carries both sides of a mutation. The two are `Option`
//! because a create has no before and a delete has no after — not because
//! recording them is optional.

pub mod resource_id;

use serde::{Deserialize, Serialize};
use settings_service_sdk::SettingKey;

pub use resource_id::AuditScope;

use crate::domain::error::DomainError;

/// What a mutation did to one setting at one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// The mutation committed.
    Success,
    /// The mutation did not commit.
    Failure,
}

/// A value as it appears in an audit record.
///
/// Secret-classified values are masked here and only here: DESIGN.md §4.2 masks
/// the record's pre/post *values*, never its resource id, so a secret setting's
/// history stays as queryable as any other while its contents never enter the
/// trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum AuditValue {
    /// A non-secret value, recorded verbatim.
    Clear(serde_json::Value),
    /// A secret-classified value. The content is deliberately absent — this
    /// variant carries no payload, so there is nothing to leak into the trail
    /// even by mistake.
    Masked,
}

impl AuditValue {
    /// Record a value, masking it when the setting is secret-classified.
    ///
    /// Taking the classification as an argument rather than inspecting the
    /// value means a caller cannot forget to mask: there is no constructor that
    /// records a value without stating whether it is secret.
    #[must_use]
    pub fn record(value: serde_json::Value, is_secret: bool) -> Self {
        if is_secret {
            Self::Masked
        } else {
            Self::Clear(value)
        }
    }
}

/// One audited mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecord {
    /// The canonical resource id, from [`resource_id::format`].
    pub resource: String,
    /// Who performed the mutation.
    pub actor: String,
    /// The action performed — create, change, revert, remove, apply, clone, or
    /// a machine secret-use.
    pub action: String,
    /// The value before, absent for a create.
    pub pre_image: Option<AuditValue>,
    /// The value after, absent for a remove.
    pub post_image: Option<AuditValue>,
    /// Whether the mutation committed.
    pub outcome: AuditOutcome,
    /// The request this mutation belonged to, for correlation.
    pub request_id: String,
}

impl AuditRecord {
    /// Start a record for a setting at a scope, with its resource id already
    /// formed by the shared formatter.
    #[must_use]
    pub fn new(
        key: &SettingKey,
        scope: AuditScope,
        actor: impl Into<String>,
        action: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            resource: resource_id::format(key, scope),
            actor: actor.into(),
            action: action.into(),
            pre_image: None,
            post_image: None,
            outcome: AuditOutcome::Success,
            request_id: request_id.into(),
        }
    }

    /// Attach the state before the mutation.
    #[must_use]
    pub fn with_pre_image(mut self, value: AuditValue) -> Self {
        self.pre_image = Some(value);
        self
    }

    /// Attach the state after the mutation.
    #[must_use]
    pub fn with_post_image(mut self, value: AuditValue) -> Self {
        self.post_image = Some(value);
        self
    }

    /// Mark the mutation as having failed.
    #[must_use]
    pub fn failed(mut self) -> Self {
        self.outcome = AuditOutcome::Failure;
        self
    }
}

/// The shared emitter every mutating feature writes through.
#[async_trait::async_trait]
pub trait AuditEmitter: Send + Sync {
    /// Write one audit record.
    ///
    /// **Synchronous and fail-closed.** A caller must propagate the error
    /// rather than continuing: a mutation whose record could not be written has
    /// no trail, and reporting it as successful would leave a change nobody can
    /// account for.
    ///
    /// # Errors
    ///
    /// [`DomainError`] when the record could not be written.
    async fn audit(&self, record: AuditRecord) -> Result<(), DomainError>;
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod audit_tests;
