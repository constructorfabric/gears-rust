//! Account Management domain error type.
//!
//! Internal-only — never crosses gear boundaries. On every boundary
//! (REST handlers, inter-gear SDK callers via `ClientHub`) this type
//! is converted to [`toolkit_canonical_errors::CanonicalError`] via
//! [`From<DomainError> for CanonicalError`], following the AIP-193 error
//! model. Public HTTP status codes and the stable error-code taxonomy
//! are defined by the canonical-errors contract; AM's role is to map
//! domain failures onto AIP-193 categories, not to invent its own
//! HTTP-status table.
//!
//! # Layering
//!
//! `DomainError` is pure — no `sea_orm::DbErr`, no `toolkit_db` types,
//! no `crate::infra` imports. The DB-aware classification ladder
//! (SQLSTATE 40001 / 23505 / availability / unclassified) lives in
//! [`crate::infra::canonical_mapping`] together with the `From` impls
//! that produce `DomainError` from raw DB errors and from `DomainError`
//! into `CanonicalError`. AM's `with_serializable_retry` wraps the raw
//! `DbErr` in an infra-internal `TxError` until the retry budget is
//! exhausted, then translates the surviving `DbErr` into a typed
//! `DomainError` (`Aborted`, `AlreadyExists`, `ServiceUnavailable`, or
//! `Internal`) before returning to the caller.

use std::time::Duration;

use thiserror::Error;
use toolkit_macros::domain_model;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// AM domain-internal error.
///
/// Variants are grouped by the AIP-193 category they map to at the
/// boundary; the grouping is preserved in declaration order so reviewers
/// can eyeball-check exhaustiveness against
/// [`From<DomainError> for CanonicalError`] in
/// [`crate::infra::canonical_mapping`].
/// Which administrative surface an [`DomainError::UnsupportedOperation`]
/// was raised against.
///
/// The three halves of `IdpPluginClient` (tenant, user, machine identity)
/// each surface "the provider does not implement this" through the same
/// domain variant. This discriminator is what survives that collapse, so
/// the boundary can stamp the correct `resource_type` on the 501 envelope
/// instead of naming one surface for all three.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedResource {
    Tenant,
    User,
    ServiceAccount,
}

// @cpt-begin:cpt-cf-account-management-dod-errors-observability-error-taxonomy-and-envelope:p1:inst-dod-error-taxonomy-enum
#[domain_model]
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DomainError {
    // ---- InvalidArgument (HTTP 400) ----
    #[error("invalid tenant type: {detail}")]
    InvalidTenantType { detail: String },

    #[error("validation failed: {detail}")]
    Validation { detail: String },

    /// Distinct from [`Self::Validation`] so the SDK boundary can
    /// route the canonical envelope to `gts.cf.core.am.tenant_metadata.v1~`
    /// instead of the tenant default. Used by metadata-content
    /// rejections (malformed chained `type_id`, null body, GTS
    /// body validation failure) — anything that describes the
    /// metadata payload rather than the owning tenant's state.
    ///
    /// `field` granularity in the canonical envelope is currently
    /// coarse — the SDK-boundary mapping in
    /// [`crate::infra::sdk_error_mapping`] hardcodes `field: "metadata"`
    /// for every raise of this variant. If a future caller needs finer
    /// `field_violation` attribution (e.g. `type_id` from
    /// `ParsedTypeId::parse`, `value` from the null-body guard),
    /// widen this variant to `{ detail, field: Option<String> }` and
    /// forward the field through the mapping.
    #[error("metadata validation failed: {detail}")]
    MetadataValidation { detail: String },

    #[error("root tenant cannot be deleted")]
    RootTenantCannotDelete,

    #[error("root tenant cannot be converted")]
    RootTenantCannotConvert,

    /// Root tenant lifecycle state is immutable from the public API
    /// (`POST /suspend` and `POST /unsuspend`). Symmetric with
    /// [`Self::RootTenantCannotDelete`]: AM-internal bootstrap owns
    /// root status; flipping it through the public surface would
    /// cascade into downstream gears that branch on
    /// `root.status` with no documented recovery path.
    #[error("root tenant status cannot be changed")]
    RootTenantCannotChangeStatus,

    /// `IdP` plugin rejected the provisioning request shape BEFORE
    /// making any external call. Distinct from [`Self::Validation`]
    /// so the SDK boundary can carry the dotted-path `field` the
    /// plugin localised the violation to (e.g.
    /// `provisioning_metadata.realm_name`) as the structured
    /// `field_violations[0].field` in the canonical envelope, with
    /// `reason = "IDP_INVALID_INPUT"`. `field = None` means the
    /// plugin couldn't localise to a specific sub-key; the canonical
    /// mapping falls back to the shared `"provisioning_metadata"`
    /// field key. Surfaces as HTTP 400 `invalid_argument`.
    #[error("IdP provider rejected request shape: {detail}")]
    IdpInvalidInput {
        detail: String,
        field: Option<String>,
    },

    /// The `IdP` rejected the supplied password against its configured
    /// password policy. Distinct from [`Self::Validation`]
    /// so the canonical envelope carries the structured
    /// `password` / `PASSWORD_POLICY` field-violation tokens instead
    /// of the generic `request` / `VALIDATION` pair — clients can
    /// attribute the failure to the password input. The raw policy
    /// text is redacted upstream (digest in `am.idp` logs); `detail`
    /// is the curated public summary. Surfaces as HTTP 400
    /// `invalid_argument`.
    #[error("password rejected by IdP policy: {detail}")]
    IdpPasswordPolicy { detail: String },

    /// The `IdP` refused to write one or more patched attributes because
    /// they are provider-managed and not overridable through AM
    /// (federated from a read-only mapper, or marked non-writable in the
    /// provider's user-profile configuration). Distinct from
    /// [`Self::UnsupportedOperation`] (the provider implements no
    /// `update_user` at all, HTTP 501) and from [`Self::Validation`]
    /// (the *value* was rejected, not the field).
    ///
    /// Carries the typed attributes only — the boundary mapping derives
    /// both each `field_violations[].field` token and the curated public
    /// detail from them, so the wording lives in exactly one place and no
    /// caller can pair an inconsistent detail/field. Surfaces as HTTP
    /// 400 `invalid_argument` with `reason = "IDP_MANAGED_FIELD"`, one
    /// violation per refused attribute, so a client learns the whole
    /// refused set from a single round-trip.
    ///
    /// `fields` is non-empty by contract; the canonical mapping degrades
    /// an empty set to the generic `request` / `VALIDATION` pair rather
    /// than emitting a 400 that attributes the refusal to nothing.
    ///
    /// Deliberately NOT 403: writability is a property of the provider's
    /// schema, identical for every caller, so no grant would make the
    /// write succeed. Modelling it as a permission denial would make a
    /// configuration fact look like a privilege problem.
    #[error("IdP-managed fields not writable ({})", fields.iter().map(|f| f.as_field_token()).collect::<Vec<_>>().join(", "))]
    IdpFieldNotWritable {
        fields: Vec<account_management_sdk::IdpUserAttribute>,
    },

    /// The `IdP` rejected a service-account request with no
    /// provider-side state retained: a name that violates the adapter's
    /// structural rules, a name already live in the tenant, a scope
    /// outside the allowlist, or a provider quota. Permanent — the same
    /// input will not start working.
    ///
    /// Distinct from [`Self::IdpInvalidInput`] (which rides the tenant
    /// resource type and carries the plugin's dotted-path `field`) on
    /// both counts: the envelope must name the service-account
    /// resource type, and it carries **no** field, because the adapter's
    /// own field attribution is untrusted text like the rest of its
    /// payload. Every provider-sourced rejection is therefore attributed
    /// to the request as a whole. `detail` is always one of the fixed
    /// messages in [`crate::domain::idp`] — never adapter text.
    /// Surfaces as HTTP 400 `invalid_argument`.
    #[error("invalid service-account request: {detail}")]
    ServiceAccountInvalidInput { detail: String },

    /// Transport uncertainty on a service-account call: the provider
    /// may have retained state, so the outcome is neither a success nor
    /// a clean failure.
    ///
    /// Mapped to HTTP 409, NOT 503. A `503` advertises "retry the same
    /// request", which for a half-applied provision would come back as
    /// [`Self::ServiceAccountInvalidInput`] ("name already live"). The
    /// 409 tells the caller to reconcile instead — list the tenant and
    /// match the submitted name — which is the only adapter-independent
    /// recovery, since client-id formats are conventions rather than
    /// contract. `detail` is the fixed reconciliation message from
    /// [`crate::domain::idp`].
    #[error("service-account outcome ambiguous: {detail}")]
    ServiceAccountAmbiguous { detail: String },

    // ---- NotFound (HTTP 404) ----
    // For every variant in this group, `resource` is the stable id
    // surfaced through the AIP-193 `NotFound` envelope's `with_resource`,
    // and `detail` is the human-readable summary populated at the call
    // site so the boundary mapping does not parse it back out.
    /// Tenant lookup miss.
    #[error("tenant not found: {detail}")]
    NotFound { detail: String, resource: String },

    /// `IdP` user lookup miss within a tenant scope.
    #[error("user not found: {detail}")]
    UserNotFound { detail: String, resource: String },

    /// Service-account lookup miss within a tenant scope — the
    /// `(tenant_id, client_id)` address does not resolve, whether
    /// because the account never existed or because it belongs to a
    /// different tenant (the two are indistinguishable to the caller by
    /// design). `revoke` folds this into success for idempotency;
    /// `rotate_secret` surfaces it, since rotating an absent account
    /// produced no credential the caller could use.
    #[error("service account not found: {detail}")]
    ServiceAccountNotFound { detail: String, resource: String },

    /// Conversion-request lookup miss.
    #[error("conversion request not found: {detail}")]
    ConversionRequestNotFound { detail: String, resource: String },

    /// Metadata lookup miss. Covers both "`type_id` unknown to the
    /// types-registry" and "schema known but no row exists at
    /// `(tenant_id, schema_uuid)`" — the two cases collapse to a
    /// single 404 per the unified-not-found contract.
    /// `entry` carries the chained `type_id` string the caller
    /// supplied so the canonical envelope can surface it as
    /// `resource_name` without exposing AM-internal compound keys
    /// like `(tenant_uuid, schema_uuid)`.
    #[error("metadata entry not found: {detail}")]
    MetadataEntryNotFound { detail: String, entry: String },

    // ---- Aborted / version mismatch (HTTP 409) ----
    /// `UpsertMetadataRequest::expected_version` did not match the
    /// stored row's `version`. Lifted to
    /// [`account_management_sdk::AccountManagementError::MetadataVersionMismatch`]
    /// at the SDK boundary and to AIP-193 `Aborted` (HTTP 409) at the
    /// canonical boundary. `entry` is the `(tenant_id, schema_uuid)`
    /// pair the caller targeted; `current` is the row's actual
    /// version (so the caller can re-issue with the right
    /// precondition); `expected` is the value the caller supplied.
    #[error("metadata version mismatch for {entry}: expected v{expected}, stored v{current}")]
    MetadataVersionMismatch {
        entry: String,
        expected: i64,
        current: i64,
    },

    // ---- AlreadyExists (HTTP 409) ----
    /// Produced by the storage layer's DB-error classifier when the
    /// underlying engine reports a unique-constraint violation
    /// (`SQLSTATE 23505` on Postgres, `SQLITE_CONSTRAINT_UNIQUE` /
    /// extended code 2067 on `SQLite`). The classifier lives in
    /// [`crate::infra::canonical_mapping`] so domain code stays free of
    /// `sea_orm` / `toolkit_db` references.
    #[error("already exists: {detail}")]
    AlreadyExists { detail: String },

    /// The `IdP` reported a uniqueness collision on a user attribute
    /// during a user operation (duplicate username in the
    /// realm, duplicate email realm-wide, or KC's combined
    /// "username or email" constant). Distinct from
    /// [`Self::AlreadyExists`] (tenant-flavoured, DB-classifier
    /// produced) so the canonical envelope rides the `user` resource
    /// type. Carries the typed colliding field — the boundary mapping
    /// derives both the stable `resource_name` token and the curated
    /// public detail from it, so the wording lives in exactly one
    /// place and no caller can pair an inconsistent detail/resource.
    /// The caller-supplied value is never echoed (redaction posture of
    /// the `IdP` boundary). Surfaces as HTTP 409 `already_exists`.
    #[error("user already exists ({})", field.as_field_token())]
    UserAlreadyExists {
        field: account_management_sdk::IdpUserDuplicateField,
    },

    // ---- Aborted (HTTP 409) ----
    /// Retry-budget-exhausted serialization failure surfaced by
    /// [`crate::infra::storage::repo_impl::helpers::with_serializable_retry`]
    /// after the underlying engine returned `SQLSTATE 40001` (or the
    /// `SQLite` analogue) for every attempt. `reason` is the canonical
    /// machine-readable token (e.g. `"SERIALIZATION_CONFLICT"`),
    /// `detail` is the human-readable summary surfaced through the
    /// canonical envelope.
    #[error("aborted: {detail}")]
    Aborted { reason: String, detail: String },

    // ---- FailedPrecondition (HTTP 400) ----
    #[error("tenant type not allowed for parent: {detail}")]
    TypeNotAllowed { detail: String },

    #[error("tenant hierarchy depth exceeded: {detail}")]
    TenantDepthExceeded { detail: String },

    #[error("tenant has child tenants")]
    TenantHasChildren,

    #[error("tenant still owns resources")]
    TenantHasResources,

    #[error("a pending conversion request already exists: {request_id}")]
    PendingExists { request_id: String },

    #[error(
        "invalid actor for conversion transition: attempted={attempted_status} caller_side={caller_side}"
    )]
    InvalidActorForTransition {
        attempted_status: String,
        caller_side: String,
    },

    #[error("conversion request already resolved")]
    AlreadyResolved,

    /// Generic precondition failure not covered by a more specific
    /// variant — used by repo paths that detect a state precondition
    /// violation (tenant deleted, type immutable, etc.) without a
    /// dedicated typed variant. Maps to
    /// [`toolkit_canonical_errors::CanonicalError::FailedPrecondition`].
    #[error("precondition failed: {detail}")]
    Conflict { detail: String },

    /// A deployment-level feature gate rejected the request. Distinct
    /// from [`Self::UnsupportedOperation`] (which signals an `IdP`
    /// plugin capability gap) so callers can distinguish configuration
    /// gates at the type level without string matching.
    #[error("feature disabled: {detail}")]
    FeatureDisabled { detail: String },

    // ---- PermissionDenied (HTTP 403) ----
    /// `cause` is `Some` only when the denial originates upstream
    /// (e.g. `From<authz_resolver_sdk::EnforcerError::Denied>`); plain
    /// AM-side ancestry rejections leave it `None`.
    #[error("cross-tenant access denied")]
    CrossTenantDenied {
        #[source]
        cause: Option<BoxError>,
    },

    // ---- ServiceUnavailable (HTTP 503) ----
    /// Covers transient infrastructure outages, generic `IdP` plugin
    /// failures, and PDP transport errors (per AIP-193). `retry_after`
    /// populates
    /// [`toolkit_canonical_errors::context::ServiceUnavailable::retry_after_seconds`]
    /// when the caller has a defensible retry budget hint.
    ///
    /// `cause` carries the upstream error chain for non-DB sources
    /// (`From<authz_resolver_sdk::EnforcerError::EvaluationFailed>`,
    /// `IdP` plugin wrappers); the DB connectivity path deliberately
    /// leaves `cause: None` to avoid leaking DSN / hostname / port
    /// fragments through `Display`.
    #[error("service unavailable: {detail}")]
    ServiceUnavailable {
        detail: String,
        retry_after: Option<Duration>,
        #[source]
        cause: Option<BoxError>,
    },

    /// `IdP` plugin reports a transient/retry-safe outage. Distinct from
    /// the generic [`Self::ServiceUnavailable`] variant because the
    /// bootstrap saga retry loop pattern-matches on this variant
    /// specifically to decide whether to keep waiting on the
    /// `idp_wait_timeout` budget vs. surfacing a fatal failure.
    /// Maps to the same AIP-193 `ServiceUnavailable` (HTTP 503) at the
    /// boundary as [`Self::ServiceUnavailable`].
    #[error("idp unavailable: {detail}")]
    IdpUnavailable { detail: String },

    // ---- Unimplemented (HTTP 501) ----
    /// Former `IdpUnsupportedOperation` — the `IdP` plugin signalled the
    /// requested administrative operation is not supported in its
    /// current implementation profile.
    ///
    /// `resource` records which administrative surface the caller
    /// addressed. All three halves of `IdpPluginClient` collapse into this
    /// one variant, so without it the boundary cannot tell a tenant call
    /// from a machine-identity call and has to guess the envelope's
    /// `resource_type` — which is the one field clients use to tell a
    /// service account apart from a user or a tenant.
    #[error("operation not supported: {detail}")]
    UnsupportedOperation {
        detail: String,
        resource: UnsupportedResource,
    },

    // ---- ResourceExhausted (HTTP 429) ----
    /// Hierarchy-integrity check refused because another check is
    /// already in progress. Maps to HTTP 429 (retry-after semantics,
    /// not a state conflict).
    ///
    /// Constructed by the storage layer (`run_integrity_check`) when
    /// the single-flight gate is held — both backends surface the
    /// conflict as a unique-violation on the `integrity_check_runs`
    /// PRIMARY KEY (`Postgres` `23505`, `SQLite` extended `2067`).
    #[error("integrity check already in progress")]
    IntegrityCheckInProgress,

    /// Hierarchy-integrity repair refused mid-tx because the AM-local
    /// lease was stolen by a peer worker between the work phase and
    /// the fence SELECT in
    /// [`crate::infra::lease::LeaseGuard::with_ack_in_tx`]. The
    /// whole repair tx rolled back; TTL handles release on the
    /// original holder's side. Distinct from
    /// [`Self::IntegrityCheckInProgress`] so the coordinator's
    /// `AbortedLeaseLost` outcome surfaces as a separate metric
    /// label without overloading the `skipped_in_progress` value.
    /// Maps to HTTP 409 at the canonical-errors boundary
    /// (mid-flight contention surfaced after partial work completed).
    #[error("integrity repair aborted: lease lost to a peer")]
    IntegrityCheckLeaseLost,

    // ---- Internal (HTTP 500) ----
    /// Unclassified internal failure. The `diagnostic` field is
    /// recorded in the audit trail but **MUST NOT** be leaked through
    /// any public `Problem` body. `cause` carries the upstream error
    /// chain when available.
    #[error("internal error")]
    Internal {
        diagnostic: String,
        #[source]
        cause: Option<BoxError>,
    },
}
// @cpt-end:cpt-cf-account-management-dod-errors-observability-error-taxonomy-and-envelope:p1:inst-dod-error-taxonomy-enum

impl DomainError {
    /// Convenience constructor for [`Self::ServiceUnavailable`] without
    /// a retry-after hint or upstream cause.
    #[must_use]
    pub fn service_unavailable(detail: impl Into<String>) -> Self {
        Self::ServiceUnavailable {
            detail: detail.into(),
            retry_after: None,
            cause: None,
        }
    }

    /// Convenience constructor for [`Self::Internal`] without an
    /// upstream cause.
    #[must_use]
    pub fn internal(diagnostic: impl Into<String>) -> Self {
        Self::Internal {
            diagnostic: diagnostic.into(),
            cause: None,
        }
    }
}

impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    /// Map PEP enforcement failures into AM's domain error model.
    ///
    /// Per the `ToolKit` `AuthZ` fail-closed invariant
    /// (`docs/toolkit_unified_system/06_authn_authz_secure_orm.md`):
    /// **denied PDP decisions, unreachable PDP, and missing /
    /// unsupported constraints all surface as 403 Forbidden** — never
    /// 500. AM follows that invariant here:
    ///
    /// - `Denied` → [`DomainError::CrossTenantDenied`] (HTTP 403). The
    ///   PDP refused the action; AM does not leak the deny reason to
    ///   the public envelope.
    /// - `EvaluationFailed` → [`DomainError::ServiceUnavailable`]
    ///   (HTTP 503). The PDP transport failed; per DESIGN §4.3
    ///   protected operations fail closed — there is no local
    ///   authorization fallback. (This is the one place `ToolKit`'s
    ///   contract diverges from a strict 403: AM exposes the
    ///   transient-outage signal via 503 + `retry_after`, while the
    ///   guarantee that the protected operation does *not* run is
    ///   identical.)
    /// - `CompileFailed` → [`DomainError::CrossTenantDenied`] (HTTP
    ///   403). A compile failure means the PDP returned a constraint
    ///   shape AM cannot enforce locally — by the fail-closed rule
    ///   that MUST be a deny, not a 500. Raw compile error is kept on
    ///   the `cause` chain for the audit trail.
    fn from(err: authz_resolver_sdk::EnforcerError) -> Self {
        use authz_resolver_sdk::EnforcerError;
        match err {
            denied @ EnforcerError::Denied { .. } => Self::CrossTenantDenied {
                cause: Some(Box::new(denied)),
            },
            EnforcerError::EvaluationFailed(source) => Self::ServiceUnavailable {
                // Generic, non-leaky detail — `source` is the AuthZ
                // Resolver SDK's transport error and can carry the
                // PDP host / port / gRPC method name in its `Display`
                // text. Operators get the raw cause from the `cause`
                // chain (audit log) and the public envelope stays clean.
                detail: "authorization evaluation failed".to_owned(),
                retry_after: None,
                cause: Some(Box::new(EnforcerError::EvaluationFailed(source))),
            },
            compile_failed @ EnforcerError::CompileFailed(_) => Self::CrossTenantDenied {
                cause: Some(Box::new(compile_failed)),
            },
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
