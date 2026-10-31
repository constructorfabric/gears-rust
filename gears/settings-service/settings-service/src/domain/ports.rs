//! Ports the write path depends on whose bindings arrive later or live in
//! infrastructure: the Secret Manager, the Change Publisher and the counters.

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::Value;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Holds `secret`-trait plaintext outside this service and hands back a
/// reference. The real binding is the Credential Store; with nothing bound a
/// write to a secret setting is refused as unavailable rather than stored in
/// plaintext, and no handle resolves.
#[async_trait]
pub trait SecretManager: Send + Sync {
    /// Mint the reference a store for the setting at `tenant` will use, unique
    /// to that write. Minted apart from the store so the caller can record it
    /// before the entry exists.
    fn mint_reference(&self, key: &str, tenant: Uuid) -> String;

    /// Store `plaintext` for the setting at `tenant` under `secret_ref`, a
    /// reference from [`Self::mint_reference`].
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when no store is bound or it cannot answer
    /// — which does not say whether the entry exists: the caller's record of
    /// the reference is what reclaims it either way.
    async fn store_secret(
        &self,
        key: &str,
        tenant: Uuid,
        secret_ref: &str,
        plaintext: &Value,
    ) -> Result<(), DomainError>;

    /// The plaintext behind `secret_ref`, stored for the setting at `tenant`,
    /// wrapped as the store hands it over: redacted in `Debug`, zeroed on drop,
    /// read only through `expose_secret()`.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] on the value when the store holds no entry,
    /// [`DomainError::Unavailable`] when it cannot answer.
    async fn resolve_plaintext(
        &self,
        key: &str,
        tenant: Uuid,
        secret_ref: &str,
    ) -> Result<SecretString, DomainError>;

    /// Release the entry behind `secret_ref`; an absent entry is already done.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the store cannot answer.
    async fn delete_secret(
        &self,
        key: &str,
        tenant: Uuid,
        secret_ref: &str,
    ) -> Result<(), DomainError>;
}

/// The binding while no Credential Store is available.
#[domain_model]
pub struct NoSecretManager;

const NO_STORE: &str = "secret values are not supported: no Secret Manager is bound";

#[async_trait]
impl SecretManager for NoSecretManager {
    fn mint_reference(&self, _key: &str, tenant: Uuid) -> String {
        format!("unbound-{tenant}")
    }

    async fn store_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
        _plaintext: &Value,
    ) -> Result<(), DomainError> {
        Err(DomainError::Unavailable {
            detail: NO_STORE.to_owned(),
        })
    }

    async fn resolve_plaintext(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
    ) -> Result<SecretString, DomainError> {
        Err(DomainError::Unavailable {
            detail: NO_STORE.to_owned(),
        })
    }

    async fn delete_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
    ) -> Result<(), DomainError> {
        Err(DomainError::Unavailable {
            detail: NO_STORE.to_owned(),
        })
    }
}

/// Decides whether a machine caller may resolve one setting's plaintext.
///
/// The decision is per setting: the resource is the value type and the
/// declaration is its id, so a service is granted the secrets it needs one at
/// a time or by a wider grant, never by holding the reader.
#[async_trait]
pub trait SecretResolveGate: Send + Sync {
    /// Refuse or permit `ctx` to resolve the declaration's plaintext.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] on the value when the decision is deny or
    /// cannot be obtained.
    async fn may_resolve(
        &self,
        ctx: &toolkit_security::SecurityContext,
        declaration_id: Uuid,
    ) -> Result<(), DomainError>;
}

/// What the write path publishes after a change is durably committed, or
/// after it was rejected.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueEvent {
    /// `event_value_changed`: a value was stored.
    Changed {
        /// The setting key.
        key: String,
        /// The scope, as a tenant id.
        tenant_id: Uuid,
        /// Who set it.
        actor: String,
        /// The change set the write belonged to.
        change_set_id: Uuid,
    },
    /// `event_declaration_registered`: a gear's declaration was stored.
    DeclarationRegistered {
        /// The setting key.
        key: String,
        /// The module that contributed it.
        actor: String,
    },
    /// `event_declaration_retired`: a declaration left resolution.
    DeclarationRetired {
        /// The setting key.
        key: String,
        /// Who retired it.
        actor: String,
    },
    /// `event_declaration_updated`: a contributed declaration's descriptive
    /// metadata changed in place on its owner's reconcile.
    ///
    /// The reconcile is the **only** path that can change these fields on a
    /// contributed declaration — the administrative edit refuses one outright
    /// (`409 ContributedDeclarationImmutable`) — and it runs unattended on
    /// every boot of the owning gear. Without this event the change would be
    /// observable nowhere: the contribution path writes no audit record
    /// (§4.2 *Module Contribution Reconciler*), so the event is the whole of
    /// the trail.
    DeclarationUpdated {
        /// The setting key.
        key: String,
        /// The module whose reconcile changed it.
        actor: String,
    },
    /// `event_declaration_reactivated`: a retired declaration is live again.
    DeclarationReactivated {
        /// The setting key.
        key: String,
        /// Who revived it.
        actor: String,
    },
    /// `event_value_change_failed`: a change was rejected, as a durable
    /// notification rather than only a response.
    ChangeFailed {
        /// The setting key.
        key: String,
        /// The scope, as a tenant id — the one requested, or the caller's own
        /// when the refusal came before the target was resolved.
        tenant_id: Uuid,
        /// Who tried.
        actor: String,
        /// Why.
        reason: String,
        /// The change set the attempt belonged to: the batch's, or the single
        /// write's own — minted before the gate, so a refusal anywhere carries
        /// it and the whole outcome of one request can be read back from the
        /// events alone.
        change_set_id: Uuid,
    },
}

/// The Change Publisher port. R1 binds no broker: the binding logs.
#[async_trait]
pub trait ChangePublisher: Send + Sync {
    /// Publish one event; never fails the write it describes.
    async fn publish(&self, event: ValueEvent);
}

/// The step-up-gated operations `settings_step_up_total` counts under.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepUpOperation {
    /// A value set.
    Set,
    /// A value reverted to its fallback.
    Revert,
    /// A value removed.
    Remove,
    /// A batch of value changes.
    Batch,
    /// A secret staged.
    Stage,
    /// Values cloned from one tenant to another.
    Clone,
    /// A declaration retired.
    Retire,
    /// A retired declaration reactivated.
    Reactivate,
    /// A declaration evolved to a successor key.
    Evolve,
    /// A declaration's behavior-affecting metadata updated.
    Update,
    /// A retired declaration purged.
    Purge,
}

impl StepUpOperation {
    /// The `operation` label value.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Revert => "revert",
            Self::Remove => "remove",
            Self::Batch => "batch",
            Self::Stage => "stage",
            Self::Clone => "clone",
            Self::Retire => "retire",
            Self::Reactivate => "reactivate",
            Self::Evolve => "evolve",
            Self::Update => "update",
            Self::Purge => "purge",
        }
    }
}

/// Counters the write path reports.
pub trait WriteMetrics: Send + Sync {
    /// `settings_value_writes_total` by result.
    fn value_write(&self, result: &'static str);
    /// `settings_step_up_total` by operation and result.
    fn step_up(&self, operation: StepUpOperation, result: &'static str);
    /// `settings_declaration_purge_secret_releases_total{result=ok}`: one
    /// Credential Store entry released after a declaration purge.
    fn purge_released(&self);
    /// `settings_declaration_purge_secret_releases_total{result=failed}`: one
    /// Credential Store entry a purge could not release.
    fn purge_release_failed(&self);
    /// `settings_changes_committed_total` by change type and outcome: the
    /// per-type breakdown of [`Self::value_write`], one call per change whose
    /// kind is known.
    fn change(&self, change_type: &'static str, outcome: &'static str);
}

/// What the managed lifecycle's passes report.
pub trait LifecycleMetrics: Send + Sync {
    /// `settings_needs_review_total` for one declaration source: the overrides
    /// flagged `needs_review` and awaiting an administrator's fix.
    fn needs_review(&self, source: &'static str, count: u64);
    /// One audit retention pass: `settings_audit_retention_passes_total` by
    /// `result` (`ok` or `failed`) and `settings_audit_records_pruned_total` by
    /// `pruned`. A failed pass and one with nothing to prune both delete
    /// nothing; this is what tells them apart.
    fn retention_pass(&self, result: &'static str, pruned: u64);
    /// `settings_service_up`: 1 while the lifecycle runs, 0 once it is told to
    /// stop.
    fn service_up(&self, up: bool);
    /// `settings_declarations_total` for one `source` and `status`.
    fn declarations(&self, source: &'static str, status: &'static str, count: u64);
    /// `settings_values_total` for one `scope_class` and `scope_kind`.
    fn values(&self, scope_class: &'static str, scope_kind: &'static str, count: u64);
}

/// What the actor name lookup of a history page reports.
pub trait ActorNameMetrics: Send + Sync {
    /// One lookup pass: `settings_actor_name_lookups_total` and
    /// `settings_actor_name_lookup_duration_seconds` by `result` (`ok`,
    /// `degraded` or `skipped`). No actor identifier is ever a label.
    fn pass(&self, result: &'static str, duration: std::time::Duration);
}

/// Reports nothing; the test binding.
#[domain_model]
pub struct NoActorNameMetrics;

impl ActorNameMetrics for NoActorNameMetrics {
    fn pass(&self, _result: &'static str, _duration: std::time::Duration) {}
}

/// What the reader boundary reports about an effective-value read.
pub trait ReadMetrics: Send + Sync {
    /// `settings_cache_lookups_total` by `result` and `scope_class`, for a
    /// lookup that resolved to a value.
    fn lookup(&self, result: &'static str, scope_class: &'static str);
    /// `settings_effective_read_duration_seconds` by `source` and
    /// `scope_class`.
    fn read(&self, source: &'static str, scope_class: &'static str, seconds: f64);
    /// `settings_resolve_failures_total` by `reason`.
    fn resolve_failure(&self, reason: &'static str);
}

/// What the enforcement points, the Secret Manager and the audit sink report.
pub trait SecurityMetrics: Send + Sync {
    /// `settings_authz_denial_total` by `operation` and `reason`.
    fn authz_denial(&self, operation: &'static str, reason: &'static str);
    /// `settings_secret_use_total` by `result`.
    fn secret_use(&self, result: &'static str);
    /// `settings_audit_events_total` by `kind`.
    fn audit_event(&self, kind: &'static str);
}

/// Counts nothing; the test binding of every metrics port.
#[domain_model]
pub struct NoMetrics;

impl WriteMetrics for NoMetrics {
    fn value_write(&self, _result: &'static str) {}
    fn step_up(&self, _operation: StepUpOperation, _result: &'static str) {}
    fn purge_released(&self) {}
    fn purge_release_failed(&self) {}
    fn change(&self, _change_type: &'static str, _outcome: &'static str) {}
}

impl LifecycleMetrics for NoMetrics {
    fn needs_review(&self, _source: &'static str, _count: u64) {}
    fn retention_pass(&self, _result: &'static str, _pruned: u64) {}
    fn service_up(&self, _up: bool) {}
    fn declarations(&self, _source: &'static str, _status: &'static str, _count: u64) {}
    fn values(&self, _scope_class: &'static str, _scope_kind: &'static str, _count: u64) {}
}

impl ReadMetrics for NoMetrics {
    fn lookup(&self, _result: &'static str, _scope_class: &'static str) {}
    fn read(&self, _source: &'static str, _scope_class: &'static str, _seconds: f64) {}
    fn resolve_failure(&self, _reason: &'static str) {}
}

impl SecurityMetrics for NoMetrics {
    fn authz_denial(&self, _operation: &'static str, _reason: &'static str) {}
    fn secret_use(&self, _result: &'static str) {}
    fn audit_event(&self, _kind: &'static str) {}
}

/// The label values the metrics ports take, each a finite set the DESIGN
/// names (§7 *Feature Metrics*): nothing a request carries — a key, a tenant,
/// an actor, a value, a path — is ever a label.
pub mod labels {
    /// `settings_changes_committed_total` / `settings_value_writes_total`.
    pub const COMMITTED: &str = "committed";
    /// A change that was refused or whose commit failed.
    pub const REJECTED: &str = "rejected";
    /// Both outcomes.
    pub const OUTCOMES: [&str; 2] = [COMMITTED, REJECTED];

    /// A stored or adopted value (`create` and `change` both count it).
    pub const SET: &str = "set";
    /// A revert to the inherited value.
    pub const REVERT: &str = "revert";
    /// A removal of the override.
    pub const REMOVE: &str = "remove";
    /// A clone of one scope's value onto another.
    pub const CLONE: &str = "clone";
    /// The kinds of change.
    pub const CHANGE_TYPES: [&str; 4] = [SET, REVERT, REMOVE, CLONE];

    /// A lookup answered from the cache.
    pub const HIT: &str = "hit";
    /// A lookup that fell through to the walk.
    pub const MISS: &str = "miss";
    /// Both lookup results.
    pub const LOOKUP_RESULTS: [&str; 2] = [HIT, MISS];

    /// A read served from the cache.
    pub const CACHE: &str = "cache";
    /// A read that resolved.
    pub const RESOLVE: &str = "resolve";
    /// Both read sources.
    pub const READ_SOURCES: [&str; 2] = [CACHE, RESOLVE];

    /// A dependency that cannot answer.
    pub const UNAVAILABLE: &str = "unavailable";
    /// A failure that is the service's own.
    pub const INTERNAL: &str = "internal";
    /// Both resolve-failure reasons.
    pub const RESOLVE_FAILURES: [&str; 2] = [UNAVAILABLE, INTERNAL];

    /// The decision point said no.
    pub const DENIED: &str = "denied";
    /// The decision point could not be reached (the same word as
    /// [`UNAVAILABLE`]).
    pub const AUTHZ_UNAVAILABLE: &str = "unavailable";
    /// The returned constraints could not be compiled.
    pub const CONSTRAINTS: &str = "constraints";
    /// The authorization refusal reasons.
    pub const AUTHZ_REASONS: [&str; 3] = [DENIED, AUTHZ_UNAVAILABLE, CONSTRAINTS];

    /// The actions an enforcement point asks for. `read_unmasked` is not among
    /// them: that entitlement is a probe, answered by masking, never counted.
    pub const AUTHZ_OPERATIONS: [&str; 6] =
        ["read", "write", "create", "update", "delete", "delegate"];

    /// A retention pass that ran to its end.
    pub const RETENTION_OK: &str = "ok";
    /// A retention pass a failure stopped early.
    pub const RETENTION_FAILED: &str = "failed";
    /// The retention pass results.
    pub const RETENTION_RESULTS: [&str; 2] = [RETENTION_OK, RETENTION_FAILED];

    /// A plaintext resolution that was handed out.
    pub const ALLOWED: &str = "allowed";
    /// The secret-use results.
    pub const SECRET_USE_RESULTS: [&str; 2] = [ALLOWED, DENIED];

    /// An audit record of a mutation (a staging included).
    pub const MUTATION: &str = "mutation";
    /// An audit record of a machine secret use.
    pub const SECRET_USE: &str = "secret_use";
    /// The audit event kinds.
    pub const AUDIT_KINDS: [&str; 2] = [MUTATION, SECRET_USE];

    /// A verified assertion.
    pub const VERIFIED: &str = "verified";
    /// A caller with no person behind it, refused for want of an assertion.
    pub const SERVICE_PRINCIPAL: &str = "service_principal";
    /// A refusal's own code, as `StepUpRefusal::code` spells it.
    pub const STEP_UP_REFUSALS: [&str; 9] = [
        "missing",
        "not_configured",
        "unavailable",
        "signature",
        "subject_mismatch",
        "auth_time_missing",
        "stale",
        "assurance",
        "claims",
    ];
}

#[cfg(test)]
#[path = "ports_tests.rs"]
mod ports_tests;
