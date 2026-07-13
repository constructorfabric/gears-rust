//! Boundary mapping from AM's internal [`DomainError`] to the AIP-193
//! [`toolkit_canonical_errors::CanonicalError`] envelope.
//!
//! Per [ADR 0005] this is the **single** authoritative classification
//! ladder for Account Management. Both surfaces consume it:
//!
//! * REST handlers convert `DomainError` to `CanonicalError` via `?`
//!   (and then to an RFC-9457 `Problem` via the platform-wide
//!   `From<CanonicalError> for Problem`).
//! * The in-process [`account_management_sdk::AccountManagementClient`]
//!   facade calls `.map_err(CanonicalError::from)`; consumers may opt
//!   into the typed `AccountManagementError` projection
//!   (`From<CanonicalError>`) at their call site.
//!
//! There is no parallel `From<DomainError> for AccountManagementError`
//! ladder — the projection is driven from `CanonicalError`, so this file
//! is the only place a domain variant is assigned a canonical category.
//!
//! # Wire vocabulary
//!
//! Every wire-string discriminator this ladder emits
//! (`field_violations[].field`/`.reason`, `violations[].subject`/`.type`,
//! `Aborted`/`PermissionDenied` `reason`, the quota `subject`) is
//! referenced from the SDK constant gears
//! ([`account_management_sdk::field`], [`account_management_sdk::precondition`],
//! [`account_management_sdk::reason`], [`account_management_sdk::quota`])
//! so the impl and the SDK projection cannot drift; the SDK's round-trip
//! `Problem` tests pin each constant to its JSON path. The
//! `#[resource_error("…")]` literals below are the one exception (proc
//! macros cannot resolve constants) and stay literals, pinned by the
//! `sdk_error_mapping_tests` canonical-envelope assertions.
//!
//! # Resource markers
//!
//! [`TenantResource`], [`UserResource`], [`TenantMetadataResource`], and
//! [`ConversionRequestResource`] are unit structs whose
//! `#[resource_error]`-generated impls produce
//! [`toolkit_canonical_errors::ResourceErrorBuilder`]s tagged with the
//! AM GTS resource types. The literal strings below MUST match the
//! corresponding `account_management_sdk::gts` constants.
//!
//! [ADR 0005]: ../../../../../../docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md

use account_management_sdk::{field, precondition, quota, reason};
use toolkit_canonical_errors::{CanonicalError, resource_error};
use tracing::warn;

use crate::domain::error::DomainError;
use crate::domain::metrics::{AM_CROSS_TENANT_DENIAL, MetricKind, emit_metric};

// ---------------------------------------------------------------------------
// Resource markers — kept in sync with account_management_sdk::gts.
// ---------------------------------------------------------------------------

#[resource_error(gts_id!("cf.core.am.tenant.v1~"))]
pub(crate) struct TenantResource;

#[resource_error(gts_id!("cf.core.am.user.v1~"))]
pub(crate) struct UserResource;

// `TenantMetadataResource` carries the unified 404 for the metadata
// surface — both "schema unknown to the registry" and "entry missing
// for this tenant" resolve to this `resource_type`. The chained
// `type_id` the caller supplied is surfaced through `resource_name`,
// so consumers still see *which* schema was involved without a separate
// type-level discriminator.
#[resource_error(gts_id!("cf.core.am.tenant_metadata.v1~"))]
pub(crate) struct TenantMetadataResource;

#[resource_error(gts_id!("cf.core.am.conversion_request.v1~"))]
pub(crate) struct ConversionRequestResource;

// ---------------------------------------------------------------------------
// DomainError → CanonicalError (the single AIP-193 ladder).
// ---------------------------------------------------------------------------
//
// One arm per `DomainError` variant, each assigned exactly one of the
// 16 canonical categories. Provider-detail redaction for `IdpUnavailable`
// / `UnsupportedOperation` happens here so the public envelope never
// carries vendor SDK text; the raw detail is logged via the `am.domain`
// `tracing` target with a digest + length only (mirrors
// [`crate::domain::idp::redact_provider_detail`]).

// @cpt-begin:cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping:p1:inst-algo-etp-domain-to-canonical
impl From<DomainError> for CanonicalError {
    #[allow(
        clippy::too_many_lines,
        reason = "flat one-arm-per-variant AIP-193 ladder; splitting it would obscure the 1:1 mapping reviewers eyeball-check against the DomainError enum"
    )]
    fn from(err: DomainError) -> Self {
        match err {
            // ---- InvalidArgument (HTTP 400) ----
            DomainError::InvalidTenantType { detail } => TenantResource::invalid_argument()
                .with_field_violation(field::TENANT_TYPE_FIELD, detail, field::INVALID_TENANT_TYPE)
                .create(),
            DomainError::Validation { detail } => TenantResource::invalid_argument()
                .with_field_violation(field::REQUEST_FIELD, detail, field::VALIDATION)
                .create(),
            // Metadata-payload validation rejects (malformed chained
            // schema id, GTS body validation failure, etc.) carry the
            // metadata resource type instead of the tenant default; both
            // are HTTP 400 `invalid_argument`.
            DomainError::MetadataValidation { detail } => {
                TenantMetadataResource::invalid_argument()
                    .with_field_violation(field::METADATA_FIELD, detail, field::VALIDATION)
                    .create()
            }
            DomainError::RootTenantCannotDelete => TenantResource::invalid_argument()
                .with_field_violation(
                    field::TENANT_ID_FIELD,
                    "root tenant cannot be deleted",
                    field::ROOT_TENANT_CANNOT_DELETE,
                )
                .create(),
            DomainError::RootTenantCannotConvert => TenantResource::invalid_argument()
                .with_field_violation(
                    field::TENANT_ID_FIELD,
                    "root tenant cannot be converted",
                    field::ROOT_TENANT_CANNOT_CONVERT,
                )
                .create(),
            DomainError::RootTenantCannotChangeStatus => TenantResource::invalid_argument()
                .with_field_violation(
                    field::TENANT_ID_FIELD,
                    "root tenant status cannot be changed",
                    field::ROOT_TENANT_CANNOT_CHANGE_STATUS,
                )
                .create(),
            // `field` is the dotted-path the IdP plugin localised the
            // violation to (e.g. `provisioning_metadata.realm_name`).
            // When the plugin can't localise (`None`) we fall back to the
            // shared `provisioning_metadata` field key — the public
            // surface every IdP plugin shares. Stays on `TenantResource`
            // (the operation being rejected is tenant provisioning).
            DomainError::IdpInvalidInput { detail, field } => TenantResource::invalid_argument()
                .with_field_violation(
                    field.unwrap_or_else(|| {
                        account_management_sdk::field::PROVISIONING_METADATA_FIELD.to_owned()
                    }),
                    detail,
                    account_management_sdk::field::IDP_INVALID_INPUT,
                )
                .create(),

            // ---- NotFound (HTTP 404) — one resource per variant ----
            DomainError::NotFound { detail, resource } => TenantResource::not_found(detail)
                .with_resource(resource)
                .create(),
            DomainError::UserNotFound { detail, resource } => UserResource::not_found(detail)
                .with_resource(resource)
                .create(),
            DomainError::ConversionRequestNotFound { detail, resource } => {
                ConversionRequestResource::not_found(detail)
                    .with_resource(resource)
                    .create()
            }
            // Both "schema unknown to registry" and "entry missing for
            // tenant" resolve to the same `TenantMetadataResource` 404;
            // `entry` carries the chained `schema_id` the caller supplied.
            DomainError::MetadataEntryNotFound { detail, entry } => {
                TenantMetadataResource::not_found(detail)
                    .with_resource(entry)
                    .create()
            }

            // ---- Aborted (HTTP 409 with reason) ----
            DomainError::MetadataVersionMismatch {
                entry,
                expected,
                current,
            } => TenantMetadataResource::aborted(format!(
                "metadata version mismatch for {entry}: expected v{expected}, stored v{current}"
            ))
            .with_resource(entry)
            .with_reason(reason::aborted::METADATA_VERSION_MISMATCH)
            .create(),
            // The domain `reason` is curated upstream but the wire token
            // is the fixed `SERIALIZATION_CONFLICT` discriminator.
            DomainError::Aborted { reason: _, detail } => TenantResource::aborted(detail)
                .with_reason(reason::aborted::SERIALIZATION_CONFLICT)
                .create(),

            // ---- AlreadyExists (HTTP 409) ----
            DomainError::AlreadyExists { detail } => TenantResource::already_exists(detail)
                .with_resource("tenant")
                .create(),
            // Duplicate-on-create per AIP-193: the at-most-one-pending
            // invariant surfaces as HTTP 409 with the existing
            // `request_id` as the structural resource identifier.
            DomainError::PendingExists { request_id } => ConversionRequestResource::already_exists(
                format!("a pending conversion request already exists: {request_id}"),
            )
            .with_resource(request_id)
            .create(),

            // ---- FailedPrecondition (HTTP 400) ----
            DomainError::TypeNotAllowed { detail } => TenantResource::failed_precondition()
                .with_precondition_violation(
                    precondition::TENANT_TYPE_SUBJECT,
                    detail,
                    precondition::TYPE_NOT_ALLOWED_TYPE,
                )
                .create(),
            DomainError::TenantDepthExceeded { detail } => TenantResource::failed_precondition()
                .with_precondition_violation(
                    precondition::DEPTH_SUBJECT,
                    detail,
                    precondition::TENANT_DEPTH_EXCEEDED_TYPE,
                )
                .create(),
            DomainError::TenantHasChildren => TenantResource::failed_precondition()
                .with_precondition_violation(
                    precondition::TENANT_SUBJECT,
                    "tenant has child tenants",
                    precondition::TENANT_HAS_CHILDREN_TYPE,
                )
                .create(),
            DomainError::TenantHasResources => TenantResource::failed_precondition()
                .with_precondition_violation(
                    precondition::TENANT_SUBJECT,
                    "tenant still owns resources",
                    precondition::TENANT_HAS_RESOURCES_TYPE,
                )
                .create(),
            DomainError::InvalidActorForTransition {
                attempted_status,
                caller_side,
            } => ConversionRequestResource::failed_precondition()
                .with_precondition_violation(
                    precondition::CONVERSION_REQUEST_SUBJECT,
                    format!(
                        "invalid actor for conversion transition: \
                         attempted={attempted_status} caller_side={caller_side}"
                    ),
                    precondition::INVALID_ACTOR_FOR_TRANSITION_TYPE,
                )
                .create(),
            DomainError::AlreadyResolved => ConversionRequestResource::failed_precondition()
                .with_precondition_violation(
                    precondition::CONVERSION_REQUEST_SUBJECT,
                    "conversion request already resolved",
                    precondition::ALREADY_RESOLVED_TYPE,
                )
                .create(),
            DomainError::Conflict { detail } => TenantResource::failed_precondition()
                .with_precondition_violation(
                    precondition::REQUEST_SUBJECT,
                    detail,
                    precondition::PRECONDITION_FAILED_TYPE,
                )
                .create(),
            DomainError::FeatureDisabled { detail } => TenantResource::failed_precondition()
                .with_precondition_violation(
                    precondition::CONFIGURATION_SUBJECT,
                    detail,
                    precondition::FEATURE_DISABLED_TYPE,
                )
                .create(),

            // ---- PermissionDenied (HTTP 403) ----
            //
            // Single funnel for every cross-tenant denial (PDP enforcer +
            // storage scope-clamp). The metadata visibility probe that
            // swallows this into `Ok(false)` bypasses this mapping, so it
            // is correctly not counted. Macro-supplied default detail
            // ("You do not have permission to perform this operation")
            // matches the pre-migration wire shape.
            DomainError::CrossTenantDenied { cause: _ } => {
                emit_metric(AM_CROSS_TENANT_DENIAL, MetricKind::Counter, &[]);
                TenantResource::permission_denied()
                    .with_reason(reason::permission::CROSS_TENANT_DENIED)
                    .create()
            }

            // ---- ResourceExhausted (HTTP 429) ----
            //
            // Not part of the inter-gear SDK contract (no
            // `AccountManagementClient` method surfaces it); routed
            // directly to the canonical 429 quota envelope.
            DomainError::IntegrityCheckInProgress => {
                TenantResource::resource_exhausted("integrity check already in progress")
                    .with_quota_violation(
                        quota::INTEGRITY_CHECK,
                        "another integrity check is already in progress",
                    )
                    .create()
            }
            // `IntegrityCheckLeaseLost` shares the public retry contract
            // with `IntegrityCheckInProgress` — both surface as the same
            // canonical 429 envelope. The metric-label split
            // (`AbortedLeaseLost` vs `SkippedInProgress`) is observed at
            // the loop-driver layer, not the REST boundary, so collapsing
            // them here keeps the public contract stable.
            DomainError::IntegrityCheckLeaseLost => {
                TenantResource::resource_exhausted("integrity repair aborted: lease lost to a peer")
                    .with_quota_violation(
                        quota::INTEGRITY_CHECK,
                        "integrity repair aborted; another worker took the lease",
                    )
                    .create()
            }

            // ---- ServiceUnavailable (HTTP 503) ----
            //
            // `IdpUnavailable` collapses to the same 503 shape as generic
            // `ServiceUnavailable` (no `retry_after_seconds` — IdP retry
            // budgets are governed by the bootstrap saga, not the wire
            // hint). Provider `detail` can carry vendor SDK strings:
            // log a digest through `am.domain` and emit a fixed envelope
            // detail.
            DomainError::IdpUnavailable { detail } => {
                let (digest, len) = crate::domain::idp::redact_provider_detail(&detail);
                warn!(
                    target: "am.domain",
                    detail_digest = digest,
                    detail_len_chars = len,
                    "IdpUnavailable surfaced; provider detail redacted for log/envelope safety"
                );
                CanonicalError::service_unavailable()
                    .with_detail("IdP plugin unavailable")
                    .create()
            }
            // `detail` is curated upstream by the adapter that produced
            // the variant (DB classifier redaction, PDP wrapper, …) and
            // is forwarded so callers see the specific outage cause.
            DomainError::ServiceUnavailable {
                detail,
                retry_after,
                cause: _,
            } => {
                let mut builder = CanonicalError::service_unavailable().with_detail(detail);
                if let Some(after) = retry_after {
                    let secs = u32::try_from(after.as_secs()).unwrap_or(u32::MAX);
                    builder = builder.with_retry_after_seconds(u64::from(secs));
                }
                builder.create()
            }

            // ---- Unimplemented (HTTP 501) ----
            DomainError::UnsupportedOperation { detail } => {
                let (digest, len) = crate::domain::idp::redact_provider_detail(&detail);
                warn!(
                    target: "am.domain",
                    detail_digest = digest,
                    detail_len_chars = len,
                    "UnsupportedOperation surfaced; provider detail redacted for log/envelope safety"
                );
                TenantResource::unimplemented("operation not supported by the IdP provider")
                    .create()
            }

            // ---- Internal (HTTP 500) ----
            DomainError::Internal {
                diagnostic,
                cause: _,
            } => CanonicalError::internal(diagnostic).create(),
        }
    }
}
// @cpt-end:cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping:p1:inst-algo-etp-domain-to-canonical

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "sdk_error_mapping_tests.rs"]
mod sdk_error_mapping_tests;
