// Created: 2026-08-13 by Constructor Tech
//! The audit trail: the record every mutation writes, and the sink that
//! commits it in the mutation's own transaction.
//!
//! Audit is a show-stopper here, not a best effort: a change the platform
//! could not record must not take effect. The sink is therefore transactional
//! and fail-closed — the record commits with the change or rolls back with it.
//! In R1 the sink is the gear's own `audit_records` table; the R2 outbox that
//! ships records onward is an addition behind the same port.

pub mod resource_id;

pub use resource_id::AuditTenant;

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The shortest default retention the store may be configured with.
pub const MIN_RETENTION_DAYS: u32 = 365;

/// Whether the mutation being recorded succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// The mutation took effect.
    Success,
    /// The mutation was attempted and refused.
    Failure,
}

impl AuditOutcome {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }

    /// From the stored spelling.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "success" => Some(Self::Success),
            "failure" => Some(Self::Failure),
            _ => None,
        }
    }
}

/// What kind of mutation a record describes — the table's closed vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOperation {
    /// A row came into existence: a category, a declaration, a first value.
    Create,
    /// A row changed in place: metadata, a value, a reactivation.
    Change,
    /// An override was cleared and the value fell back.
    Revert,
    /// A row was removed or retired.
    Remove,
    /// A value was copied from another scope.
    Clone,
    /// A machine caller resolved a secret's plaintext.
    SecretUse,
}

impl AuditOperation {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Change => "change",
            Self::Revert => "revert",
            Self::Remove => "remove",
            Self::Clone => "clone",
            Self::SecretUse => "secret_use",
        }
    }

    /// From the stored spelling.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "create" => Some(Self::Create),
            "change" => Some(Self::Change),
            "revert" => Some(Self::Revert),
            "remove" => Some(Self::Remove),
            "clone" => Some(Self::Clone),
            "secret_use" => Some(Self::SecretUse),
            _ => None,
        }
    }
}

/// The actor identity is itself classified: an administrator's identity is
/// PII, a contributing module's name is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorClassification {
    /// Shown to every reader.
    Public,
    /// Masked for a reader without the PII entitlement.
    Pii,
}

impl ActorClassification {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Pii => "pii",
        }
    }

    /// From the stored spelling.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "public" => Some(Self::Public),
            "pii" => Some(Self::Pii),
            _ => None,
        }
    }
}

/// A pre- or post-image as it goes into the trail.
///
/// Secret-classified values are masked here and only here: DESIGN.md §4.2 masks
/// them before the record is built, so no later stage ever holds plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum AuditValue {
    /// A value recorded as it was.
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
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-1
        if is_secret {
            Self::Masked
        } else {
            Self::Clear(value)
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-1
    }
}

/// One audit record, as a mutation hands it to the sink.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecord {
    /// The canonical resource id, `cf.settings:{key}@{tenant_id}`.
    pub resource: String,
    /// The setting key, denormalized from `resource` for the scoped query.
    pub declaration_key: String,
    /// The scope as an id; the root tenant's id is platform scope.
    pub tenant_id: Uuid,
    /// What happened.
    pub operation: AuditOperation,
    /// Who did it.
    pub actor: String,
    /// How the actor identity is classified for the read side.
    pub actor_classification: ActorClassification,
    /// The value before, masked when secret.
    pub pre_image: Option<AuditValue>,
    /// The value after, masked when secret.
    pub post_image: Option<AuditValue>,
    /// Whether the mutation succeeded.
    pub outcome: AuditOutcome,
    /// The request that produced it.
    pub request_id: String,
    /// The change set the mutation was produced under, when one applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_set_id: Option<Uuid>,
    /// An explicit retention horizon; absent, the store's default applies.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub retain_until: Option<OffsetDateTime>,
}

impl AuditRecord {
    /// A successful mutation of `key` at `tenant` by an administrator.
    ///
    /// The actor is classified `pii`: an administrator identity is personal
    /// data. A module actor calls [`Self::by_module`].
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        tenant: AuditTenant,
        actor: impl Into<String>,
        operation: AuditOperation,
        request_id: impl Into<String>,
    ) -> Self {
        let key = key.into();
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-3
        // `resource`, `declaration_key` and `tenant_id` come from the same two
        // inputs, so the indexed pair can never disagree with the id.
        let resource = resource_id::format_raw(&key, tenant);
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-3
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-2
        Self {
            resource,
            declaration_key: key,
            tenant_id: tenant,
            operation,
            actor: actor.into(),
            actor_classification: ActorClassification::Pii,
            pre_image: None,
            post_image: None,
            outcome: AuditOutcome::Success,
            request_id: request_id.into(),
            change_set_id: None,
            retain_until: None,
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-2
    }

    /// The actor is a contributing module, whose name is not personal data.
    #[must_use]
    pub fn by_module(mut self) -> Self {
        self.actor_classification = ActorClassification::Public;
        self
    }

    /// Attach the value before.
    #[must_use]
    pub fn with_pre_image(mut self, value: AuditValue) -> Self {
        self.pre_image = Some(value);
        self
    }

    /// Attach the value after.
    #[must_use]
    pub fn with_post_image(mut self, value: AuditValue) -> Self {
        self.post_image = Some(value);
        self
    }

    /// Attach the change set the mutation was produced under.
    #[must_use]
    pub fn with_change_set(mut self, change_set_id: Uuid) -> Self {
        self.change_set_id = Some(change_set_id);
        self
    }

    /// Give the record an explicit retention horizon.
    #[must_use]
    pub fn with_retain_until(mut self, at: OffsetDateTime) -> Self {
        self.retain_until = Some(at);
        self
    }

    /// Mark the mutation as refused.
    #[must_use]
    pub fn failed(mut self) -> Self {
        self.outcome = AuditOutcome::Failure;
        self
    }
}

/// A record as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAuditRecord {
    /// Row identity.
    pub id: Uuid,
    /// The setting key.
    pub declaration_key: String,
    /// The scope as an id.
    pub tenant_id: Uuid,
    /// What happened.
    pub operation: AuditOperation,
    /// Who did it.
    pub actor: String,
    /// How the actor identity is classified.
    pub actor_classification: ActorClassification,
    /// The value before.
    pub pre_image: Option<AuditValue>,
    /// The value after.
    pub post_image: Option<AuditValue>,
    /// Whether the mutation succeeded.
    pub outcome: AuditOutcome,
    /// The request that produced it.
    pub request_id: String,
    /// The change set, when one applies.
    pub change_set_id: Option<Uuid>,
    /// When it happened — the transaction's clock.
    pub occurred_at: OffsetDateTime,
    /// The explicit retention horizon, when one was given.
    pub retain_until: Option<OffsetDateTime>,
}

/// The sink every mutation writes through.
///
/// `append` runs inside the caller's transaction as its last step before
/// commit. It is the only way a record is written, so the R2 outbox binding is
/// added behind it without touching a call site.
// @cpt-dod:cpt-cf-settings-service-dod-gear-foundation-audit-emitter:p1
#[async_trait::async_trait]
pub trait AuditSink: Send + Sync {
    /// Write one record in the caller's transaction.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the record cannot be written; the
    /// caller must propagate it so the transaction rolls back — a mutation
    /// whose record could not be written has no trail, and reporting it as
    /// successful would leave a change nobody can account for.
    async fn append<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        record: AuditRecord,
    ) -> Result<(), DomainError>;
}

/// The instant a record leaves its online window.
#[must_use]
pub fn retention_horizon(
    retain_until: Option<OffsetDateTime>,
    occurred_at: OffsetDateTime,
    default_retention: Duration,
) -> OffsetDateTime {
    // @cpt-begin:cpt-cf-settings-service-algo-audit-store-retention:p1:inst-as-ret-1
    // @cpt-begin:cpt-cf-settings-service-algo-audit-store-retention:p1:inst-as-ret-2
    // @cpt-begin:cpt-cf-settings-service-algo-audit-store-retention:p1:inst-as-ret-4
    // An explicit horizon is that instant; otherwise the configured default
    // counted from when the record was written. Shipping onward in R2 copies
    // a record and changes nothing about its window here.
    retain_until.unwrap_or(occurred_at + default_retention)
    // @cpt-end:cpt-cf-settings-service-algo-audit-store-retention:p1:inst-as-ret-4
    // @cpt-end:cpt-cf-settings-service-algo-audit-store-retention:p1:inst-as-ret-2
    // @cpt-end:cpt-cf-settings-service-algo-audit-store-retention:p1:inst-as-ret-1
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod audit_tests;
