//! `IdP` provider-plugin contract.
//!
//! Public contract that deployment-specific `IdP` plugins implement and
//! that AM consumes through `ClientHub`. The trait carries two
//! tenant-lifecycle methods ([`IdpPluginClient::provision_tenant`],
//! [`IdpPluginClient::deprovision_tenant`]) and three user-lifecycle
//! methods ([`IdpPluginClient::provision_user`],
//! [`IdpPluginClient::deprovision_user`],
//! [`IdpPluginClient::list_users`]) — together with the
//! request / result / failure shapes they exchange. Every method
//! ships a default impl that returns the `UnsupportedOperation`
//! variant for its category so deployments that ship a partial
//! adapter (e.g. tenant-only or user-only) only need to override the
//! methods they implement.
//!
//! There is no separate availability probe: `provision_tenant` IS the
//! readiness signal. Plugins MUST return
//! [`IdpProvisionFailure::CleanFailure`] for failures that proved no
//! `IdP`-side state was retained, and [`IdpProvisionFailure::Ambiguous`]
//! for uncertain outcomes. AM's saga handles retry vs reaper-deferral
//! per variant.
//!
//! The trait runs **outside** any database transaction — the
//! provisioning step is an external side effect that must not hold
//! locks in `tenants`.
//!
//! # Plugin-private metadata
//!
//! AM is a stateful echo proxy for plugin-owned per-tenant data.
//! [`IdpProvisionResult::metadata`] is an opaque blob the plugin returns
//! on successful provisioning; AM persists it in `tenant_idp_metadata`
//! keyed by `tenant_id` and replays it on every subsequent `IdP` call
//! for that tenant via [`crate::idp_user::IdpTenantContext::metadata`]
//! and [`IdpDeprovisionTenantRequest::tenant_context`]. AM does NOT
//! interpret, validate, or namespace the value — the plugin owns the
//! shape entirely. Size is capped at the AM service boundary.
//!
//! # Failure model
//!
//! The `Ok` variant of `provision_tenant` carries optional opaque
//! metadata produced by the provider, which AM persists alongside the
//! `active` status flip. The `Err` variant is a [`IdpProvisionFailure`]
//! discriminating between:
//!
//! * [`IdpProvisionFailure::CleanFailure`] — AM can prove no `IdP`-side
//!   state was retained (connection refused before send, 4xx from the
//!   provider with a contract-defined "nothing retained" semantic).
//!   AM runs the compensating TX, deletes the `provisioning` row, and
//!   surfaces an AIP-193 `ServiceUnavailable` (HTTP 503).
//! * [`IdpProvisionFailure::Ambiguous`] — transport failure / timeout /
//!   5xx where the provider may or may not have retained state. AM
//!   leaves the `provisioning` row for the provisioning reaper to
//!   compensate asynchronously and surfaces `Internal` (HTTP 500). Not
//!   retry-safe without reconciliation.
//! * [`IdpProvisionFailure::UnsupportedOperation`] — the provider
//!   signalled that the requested provisioning cannot be performed at
//!   all. AM surfaces `Unimplemented` (HTTP 501); compensation rules
//!   match the `CleanFailure` path (nothing was ever written
//!   provider-side).
//!
//! The `provider_detail` strings carried by the failure variants are
//! routed through AM's redaction pipeline before reaching public
//! envelopes (see the impl-side `From<IdpProvisionFailure>
//! for DomainError` boundary in `cyberware-account-management::domain::idp`).
//! Plugin authors do not need to redact themselves — they pass the
//! raw vendor text and AM owns the public-surface mapping.
//!
//! See [`crate::idp_user`] for the user-operations DTOs forwarded
//! into the same trait's user-side methods.

use async_trait::async_trait;
use gts::GtsSchemaId;
use serde_json::Value;
use uuid::Uuid;

use modkit_odata::Page;

use crate::idp_user::{
    IdpDeprovisionUserRequest, IdpListUsersRequest, IdpProvisionUserRequest, IdpTenantContext,
    IdpUser, IdpUserOperationFailure,
};

/// Whether the tenant being provisioned is the platform root or a
/// regular child. Lives inside [`IdpProvisionTenantRequest::target`] so
/// the root-bootstrap branch is expressed by an explicit named variant
/// rather than the absence of a parent id (which previously read as
/// "missing data" rather than "canonical bootstrap signal").
///
/// Call-sites do not construct this enum directly — they go through
/// [`IdpProvisionTenantRequest::for_root`] (bootstrap) or
/// [`IdpProvisionTenantRequest::new`] (steady-state child). The variant
/// remains in the public surface so plugin authors that want to
/// branch on the target inside `provision_tenant` can match on it.
///
/// `#[non_exhaustive]` lets the SDK introduce a new topology
/// (e.g. impersonation, shadow, sub-tenant) in a minor release
/// without breaking compilation on plugins that exhaustively
/// matched the two current variants. Plugins MUST include a `_`
/// arm and surface unknown variants as
/// [`IdpProvisionFailure::UnsupportedOperation`] until they explicitly
/// add support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdpProvisionTarget {
    /// Platform-root bootstrap. Emitted exactly once per deployment
    /// by [`account_management_sdk`]'s bootstrap saga.
    Root,
    /// Regular child tenant under `parent_id`.
    Child { parent_id: Uuid },
}

/// Context passed to [`IdpPluginClient::provision_tenant`].
///
/// Carries the identifiers and opaque provider metadata produced during
/// the pre-provisioning validation step. The `tenant_type` here is the
/// full chained GTS identifier (DESIGN §3.1 "Input and storage
/// format"); `target` distinguishes the canonical root-bootstrap path
/// ([`IdpProvisionTarget::Root`]) from steady-state child creation
/// ([`IdpProvisionTarget::Child { parent_id }`]).
///
/// Construct via [`Self::for_root`] (once-per-deployment bootstrap)
/// or [`Self::new`] (steady-state child tenant — the everyday path).
/// `IdpProvisionTarget` never appears in call-sites — only inside plugin
/// impls that choose to match on `req.target`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct IdpProvisionTenantRequest {
    pub tenant_id: Uuid,
    pub target: IdpProvisionTarget,
    pub name: String,
    /// Full chained GTS schema identifier (e.g.
    /// `gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~`).
    /// Typed via [`GtsSchemaId`] rather than `String` so the field
    /// is self-describing for plugin authors and surfaces with
    /// `format: gts-schema-id` in any generated JSON Schema. The
    /// wire shape stays a string; AM-side consumers run full chain
    /// validation by passing the value through `gts::GtsID::new`.
    pub tenant_type: GtsSchemaId,
    /// Opaque provider-specific metadata from `TenantCreateRequest.provisioning_metadata`.
    pub metadata: Option<Value>,
}

impl IdpProvisionTenantRequest {
    /// Construct a request for the platform-root bootstrap path.
    /// Emitted exactly once per deployment by the bootstrap saga;
    /// every steady-state tenant creation uses [`Self::new`] instead.
    #[must_use]
    pub fn for_root(tenant_id: Uuid, name: impl Into<String>, tenant_type: GtsSchemaId) -> Self {
        Self {
            tenant_id,
            target: IdpProvisionTarget::Root,
            name: name.into(),
            tenant_type,
            metadata: None,
        }
    }

    /// Construct a request for a child tenant under `parent_id` —
    /// the everyday steady-state path used by the create-child-tenant
    /// saga. For the once-per-deployment platform-root bootstrap use
    /// [`Self::for_root`].
    #[must_use]
    pub fn new(
        tenant_id: Uuid,
        parent_id: Uuid,
        name: impl Into<String>,
        tenant_type: GtsSchemaId,
    ) -> Self {
        Self {
            tenant_id,
            target: IdpProvisionTarget::Child { parent_id },
            name: name.into(),
            tenant_type,
            metadata: None,
        }
    }

    /// Builder-style setter for [`Self::metadata`].
    #[must_use]
    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Result returned by the provider on successful tenant provisioning.
///
/// Carries an opaque plugin-private metadata blob that AM persists in
/// `tenant_idp_metadata` and replays on every subsequent `IdP` call for
/// this tenant via [`crate::idp_user::IdpTenantContext::metadata`] and
/// [`IdpDeprovisionTenantRequest::tenant_context`]. The shape of the value
/// is owned entirely by the plugin — AM does not interpret, validate,
/// or namespace it. `None` means the plugin owns no per-tenant state
/// (typical for providers that bind via external configuration or
/// naming convention).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct IdpProvisionResult {
    pub metadata: Option<Value>,
}

impl IdpProvisionResult {
    /// Construct a result carrying the given opaque metadata blob.
    /// Use [`Default::default`] for the "no metadata returned" path.
    #[must_use]
    pub const fn new(metadata: Option<Value>) -> Self {
        Self { metadata }
    }
}

/// Failure discriminant for `provision_tenant`.
///
/// See module docs for compensation semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdpProvisionFailure {
    /// AM can prove no `IdP`-side state was retained. Triggers the
    /// compensating TX that deletes the `provisioning` row.
    CleanFailure { detail: String },
    /// Outcome is uncertain; provider may have retained state. The
    /// provisioning reaper compensates asynchronously.
    Ambiguous { detail: String },
    /// Provider does not support the requested provisioning at all.
    /// Surfaces as `idp_unsupported_operation`.
    UnsupportedOperation { detail: String },
}

impl IdpProvisionFailure {
    /// Stable, snake-case metric-label form of this variant. Used as
    /// the `outcome` label on `am.dependency_health` counter samples
    /// emitted by the create-tenant saga; kept on the SDK type so
    /// producers (impl-side service layer) do not duplicate the
    /// variant → string mapping in match arms.
    #[must_use]
    pub const fn as_metric_label(&self) -> &'static str {
        match self {
            Self::CleanFailure { .. } => "clean_failure",
            Self::Ambiguous { .. } => "ambiguous",
            Self::UnsupportedOperation { .. } => "unsupported_operation",
        }
    }

    /// Raw provider-supplied `detail` string carried by every variant.
    /// Consumers (audit pipeline, structured logging, redaction) read
    /// the detail uniformly across all `IdP` failure enums.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::CleanFailure { detail }
            | Self::Ambiguous { detail }
            | Self::UnsupportedOperation { detail } => detail,
        }
    }
}

impl core::fmt::Display for IdpProvisionFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.as_metric_label(), self.detail())
    }
}

impl core::error::Error for IdpProvisionFailure {}

/// Context passed to [`IdpPluginClient::deprovision_tenant`]
/// during the hard-delete pipeline or the provisioning reaper.
///
/// Carries the unified [`IdpTenantContext`] so the plugin receives the
/// same tenant snapshot (`tenant_id`, `tenant_name`, `tenant_type`,
/// opaque plugin-private `metadata`) as on every user-ops call. The
/// `metadata` field lets the plugin recover its own binding state
/// (realm name, vendor org id, …) before tearing down vendor-side
/// resources, symmetric with how `provision_user` / `list_users` /
/// `deprovision_user` consume it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct IdpDeprovisionTenantRequest {
    pub tenant_context: IdpTenantContext,
}

impl IdpDeprovisionTenantRequest {
    #[must_use]
    pub const fn new(tenant_context: IdpTenantContext) -> Self {
        Self { tenant_context }
    }
}

/// Failure discriminant for `deprovision_tenant`.
///
/// `Terminal` means the tenant cannot be deprovisioned by this
/// provider and the operator must intervene; `Retryable` defers to the
/// next tick; `UnsupportedOperation` is the default path that
/// preserves Phase 1/2 behaviour when no provider plugin is
/// registered. `NotFound` is the "vendor-side already gone" path — AM
/// treats it as a success-equivalent and proceeds with the local DB
/// teardown (see the trait-level doc on idempotency vs typed errors).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdpDeprovisionFailure {
    /// Non-recoverable; logs/audits and skips the tenant this tick.
    Terminal { detail: String },
    /// Transient; defer the tenant to the next retention tick.
    Retryable { detail: String },
    /// Provider does not support deprovisioning at all.
    UnsupportedOperation { detail: String },
    /// Provider reports the target tenant does not exist on its side
    /// (e.g. HTTP 404 / 410 from the vendor SDK). AM treats this as a
    /// success-equivalent — the local DB teardown still runs — and
    /// emits an `outcome=already_absent` metric so the operational
    /// signal is observable distinct from a fresh `compensated`.
    NotFound { detail: String },
}

impl IdpDeprovisionFailure {
    /// Stable, snake-case metric-label form of this variant. Used as
    /// the `outcome` label on `am.dependency_health` counter samples
    /// emitted by the hard-delete pipeline; kept on the SDK type so
    /// producers (impl-side service layer) do not duplicate the
    /// variant → string mapping in match arms.
    #[must_use]
    pub const fn as_metric_label(&self) -> &'static str {
        match self {
            Self::Terminal { .. } => "terminal",
            Self::Retryable { .. } => "retryable",
            Self::UnsupportedOperation { .. } => "unsupported_operation",
            Self::NotFound { .. } => "already_absent",
        }
    }

    /// Raw provider-supplied `detail` string carried by every variant.
    /// Mirrors [`IdpProvisionFailure::detail`] so consumers read the
    /// detail uniformly across all `IdP` failure enums.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Terminal { detail }
            | Self::Retryable { detail }
            | Self::UnsupportedOperation { detail }
            | Self::NotFound { detail } => detail,
        }
    }
}

impl core::fmt::Display for IdpDeprovisionFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.as_metric_label(), self.detail())
    }
}

impl core::error::Error for IdpDeprovisionFailure {}

/// Trait implemented by the deployment-specific `IdP` provider plugin.
///
/// Single combined contract carrying both tenant-lifecycle and
/// user-lifecycle methods. Every method ships a default impl that
/// returns the `UnsupportedOperation` variant for its category, so
/// deployments that ship a partial adapter (tenant-only, user-only,
/// or read-only directory) compile without stubbing every method
/// explicitly.
///
/// # Retry, backoff, and rate-limiting are owned by the plugin
///
/// AM does NOT wrap calls into this trait in retry loops, exponential
/// backoff, jittered scheduling, or circuit-breakers. Each AM call
/// site issues exactly one invocation per logical attempt:
///
/// * `provision_tenant` — one call per `create_tenant` saga.
/// * `deprovision_tenant` — at most one call per claimed row per
///   tick (both `hard_delete_batch` and `reap_stuck_provisioning`
///   take a 600-second DB lease before invoking the plugin so two
///   replicas cannot simultaneously call for the same tenant).
/// * `provision_user` / `deprovision_user` / `list_users` — exactly
///   one call per public REST request (or sibling SDK consumer).
///
/// Plugins MUST own their transport-level resilience: retries with
/// vendor-appropriate backoff, ratelimit handling, circuit breaking
/// after sustained failure, and any client-side dedup. A `Retryable`
/// return value signals that the plugin has exhausted its own retry
/// budget for this call; AM defers the row to the next reaper /
/// retention tick (default 30 s and 60 s respectively) and re-issues
/// from scratch. A misbehaving plugin that does not ratelimit will
/// see a steady periodic call rate (one per tick), not a thundering
/// herd — but the call frequency is the plugin's to manage.
///
/// # No silent no-op on mutating calls
///
/// `provision_tenant`, `deprovision_tenant`, `provision_user`, and
/// `deprovision_user` MUST NOT silently no-op. A provider that
/// cannot perform a mutating operation MUST return the
/// `UnsupportedOperation` variant for its failure category so AM
/// surfaces `idp_unsupported_operation` (HTTP 501) per PRD section
/// 5.5 and DESIGN section 3.8.
///
/// # Idempotency by error-mapping
///
/// Plugins do NOT need to silently no-op on already-removed tenants.
/// Instead they MUST surface vendor-side "tenant does not exist"
/// responses as [`IdpDeprovisionFailure::NotFound`] (typically HTTP 404
/// or 410 from the vendor SDK). AM's pipelines treat `NotFound` as
/// success-equivalent and proceed with the local DB teardown,
/// emitting an `outcome=already_absent` metric so the operational
/// signal stays observable. This shifts the "is this a re-call?"
/// interpretation from the plugin to AM — plugins map vendor errors
/// 1:1, AM business logic decides what each error means.
///
/// The user-side `deprovision_user` is intentionally simpler:
/// returning `Ok(())` is enough whether the `IdP` actually removed
/// the user on this call or the user was already absent. AM does
/// not distinguish the two cases (no reaper-style counter
/// equivalent exists on the user side; the audit log records the
/// call without branching on removed-vs-absent), so forcing every
/// plugin author to thread an extra return variant only adds noise.
///
/// # `ClientHub` registration
///
/// Plugins register themselves in `ClientHub` as
/// `Arc<dyn IdpPluginClient>`; AM's module entry-point
/// resolves the plugin via
/// `ctx.client_hub().get::<dyn IdpPluginClient>()` and
/// falls back to a noop provisioner when no plugin is registered (dev
/// / test deployments).
#[async_trait]
pub trait IdpPluginClient: Send + Sync + 'static {
    // ---- Tenant lifecycle -----------------------------------------

    /// Create any `IdP`-side resources for the new tenant.
    ///
    /// Invariants:
    /// * Runs outside any DB transaction.
    /// * MUST NOT silently no-op — provider implementations that
    ///   cannot perform the operation MUST return
    ///   [`IdpProvisionFailure::UnsupportedOperation`].
    /// * Any transport-layer uncertainty MUST be reported as
    ///   [`IdpProvisionFailure::Ambiguous`]; the provider MUST NOT
    ///   pretend a timed-out request succeeded.
    /// * MUST own retry / backoff / rate-limiting policy (see trait-
    ///   level doc). AM issues exactly one call per saga attempt.
    async fn provision_tenant(
        &self,
        req: &IdpProvisionTenantRequest,
    ) -> Result<IdpProvisionResult, IdpProvisionFailure> {
        let _ = req;
        Err(IdpProvisionFailure::UnsupportedOperation {
            detail: "provision_tenant not implemented".to_owned(),
        })
    }

    /// Tear down `IdP`-side resources attached to the tenant.
    ///
    /// Default impl returns
    /// [`IdpDeprovisionFailure::UnsupportedOperation`]. Providers that
    /// own teardown MUST override this method.
    ///
    /// Invariants:
    /// * MUST map vendor-side "tenant does not exist" responses
    ///   (typically HTTP 404 / 410) to
    ///   [`IdpDeprovisionFailure::NotFound`]. AM uses this signal as a
    ///   success-equivalent — the local DB teardown still proceeds —
    ///   so plugins do NOT need to magic-map "already gone" into
    ///   `Ok(())` themselves. Idempotency by error-mapping is the
    ///   contract.
    /// * MUST own retry / backoff / rate-limiting policy (see trait-
    ///   level doc). AM issues at most one call per reaper /
    ///   retention tick per row (rows are claimed via the same
    ///   600-second DB lease that the retention pipeline uses), so a
    ///   `Retryable` return defers the row to the next tick.
    async fn deprovision_tenant(
        &self,
        req: &IdpDeprovisionTenantRequest,
    ) -> Result<(), IdpDeprovisionFailure> {
        let _ = req;
        Err(IdpDeprovisionFailure::UnsupportedOperation {
            detail: "deprovision_tenant not implemented".to_owned(),
        })
    }

    // ---- IdpUser lifecycle -------------------------------------------
    // @cpt-begin:cpt-cf-account-management-dod-idp-user-operations-contract-contract-trait-surface:p1:inst-trait-user-ops-surface

    /// Provision a user in the supplied tenant scope.
    ///
    /// On success the provider returns the `IdP`-assigned
    /// [`IdpUser`] (the `id` field is the authoritative,
    /// `IdP`-issued user UUID).
    ///
    /// Default impl returns
    /// [`IdpUserOperationFailure::UnsupportedOperation`] so tenant-only
    /// adapters compile without stubbing user methods.
    async fn provision_user(
        &self,
        req: &IdpProvisionUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        let _ = req;
        Err(IdpUserOperationFailure::UnsupportedOperation {
            detail: "provision_user not implemented".to_owned(),
        })
    }

    /// Deprovision a user in the supplied tenant scope. Removes any
    /// active sessions where the provider supports session
    /// revocation.
    ///
    /// Returns `Ok(())` both when the user existed and was removed
    /// and when the user was already absent — the contract is "the
    /// user is gone in this tenant scope after the call". Plugins
    /// MUST map vendor-side "user does not exist" responses
    /// (typically HTTP 404 / 410 from the `IdP` SDK) to `Ok(())`, not
    /// to `Err`, so `DELETE /tenants/{tenant_id}/users/{user_id}`
    /// stays retry-safe per
    /// `cpt-cf-account-management-fr-idp-user-deprovision`.
    ///
    /// Default impl returns
    /// [`IdpUserOperationFailure::UnsupportedOperation`].
    async fn deprovision_user(
        &self,
        req: &IdpDeprovisionUserRequest,
    ) -> Result<(), IdpUserOperationFailure> {
        let _ = req;
        Err(IdpUserOperationFailure::UnsupportedOperation {
            detail: "deprovision_user not implemented".to_owned(),
        })
    }

    /// List users in the supplied tenant scope, optionally filtered to
    /// a single `user_id`. Cursor-based pagination via
    /// [`crate::IdpUserPagination`]: AM forwards an opaque
    /// `Option<String>` cursor on the request and returns
    /// `modkit_odata::Page<IdpUser>` whose `page_info.next_cursor` is
    /// the plugin-shaped token for the following page (`None` when
    /// the listing is exhausted).
    ///
    /// AM never inspects the cursor — the plugin owns its format.
    /// Plugins backed by a SQL store SHOULD encode a filter hash and
    /// a stable sort key (see [`modkit_odata::pagination`]) so a
    /// client switching `user_id_filter` mid-pagination receives a
    /// deterministic invalid-cursor error rather than silently
    /// jumping pages; vendor-SDK plugins MAY forward a native cursor
    /// token (e.g. Zitadel `next_token`) unchanged.
    ///
    /// A `user_id_filter = Some(_)` returning an empty page is the
    /// authoritative existence signal AM consumes for downstream
    /// features (e.g. `feature-user-groups` membership checks); both
    /// the one-element and empty outcomes are success.
    ///
    /// Default impl returns
    /// [`IdpUserOperationFailure::UnsupportedOperation`].
    async fn list_users(
        &self,
        req: &IdpListUsersRequest,
    ) -> Result<Page<IdpUser>, IdpUserOperationFailure> {
        let _ = req;
        Err(IdpUserOperationFailure::UnsupportedOperation {
            detail: "list_users not implemented".to_owned(),
        })
    }
    // @cpt-end:cpt-cf-account-management-dod-idp-user-operations-contract-contract-trait-surface:p1:inst-trait-user-ops-surface
}

#[cfg(test)]
#[path = "idp_tests.rs"]
mod tests;
