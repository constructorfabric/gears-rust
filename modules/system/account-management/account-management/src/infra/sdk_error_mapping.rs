//! Boundary mapping between AM's internal [`DomainError`] and the two
//! public error envelopes AM emits:
//!
//! * [`AccountManagementError`] (`account-management-sdk`) — the
//!   typed error surface inter-module Rust callers see through
//!   [`account_management_sdk::AccountManagementClient`] (resolved
//!   via `ClientHub`). Variant-per-failure shape (mirrors `mini-chat`
//!   convention) — each enum case names the failure semantically.
//! * [`modkit_canonical_errors::CanonicalError`] — the AIP-193
//!   envelope REST handlers convert to RFC-9457 `Problem` responses.
//!
//! Both targets share the same upstream `DomainError`; the SDK lift
//! happens through the `From<DomainError> for AccountManagementError`
//! impl below and the REST lift through
//! [`account_management_error_to_canonical`] (free fn — both types
//! foreign to this crate, orphan rule blocks an impl block). The
//! `From<DomainError> for CanonicalError` at the bottom composes the
//! two hops so REST handlers can write `service.foo().await?`.
//!
//! # Resource markers
//!
//! [`TenantResource`], [`TenantMetadataResource`], and
//! [`ConversionRequestResource`] are unit structs whose
//! `#[resource_error]`-generated impls produce
//! [`modkit_canonical_errors::ResourceErrorBuilder`]s tagged with the
//! AM GTS resource types. The literal strings below MUST match the
//! corresponding constants in `account_management_sdk::gts`; the
//! `domain::error_tests::resource_error_strings_match_sdk_constants`
//! test asserts equality at test time so a divergence trips there, not
//! in production.

use account_management_sdk::error::AccountManagementError;
use modkit_canonical_errors::{CanonicalError, resource_error};
use tracing::warn;

use crate::domain::error::DomainError;

// ---------------------------------------------------------------------------
// Resource markers — kept in sync with account_management_sdk::gts via
// `domain::error_tests::resource_error_strings_match_sdk_constants`.
// ---------------------------------------------------------------------------

#[resource_error("gts.cf.core.am.tenant.v1~")]
pub(crate) struct TenantResource;

#[resource_error("gts.cf.core.am.user.v1~")]
pub(crate) struct UserResource;

#[resource_error("gts.cf.core.am.tenant_metadata.v1~")]
pub(crate) struct TenantMetadataResource;

#[resource_error("gts.cf.core.am.conversion_request.v1~")]
pub(crate) struct ConversionRequestResource;

// ---------------------------------------------------------------------------
// DomainError → AccountManagementError (SDK boundary).
// ---------------------------------------------------------------------------
//
// Direct semantic mapping — each `DomainError` variant lifts to a
// uniquely-named `AccountManagementError` variant. The provider-detail
// redaction for `IdpUnavailable` / `UnsupportedOperation` happens here
// so the public detail that reaches the SDK never carries vendor SDK
// text; raw detail is logged via the `am.domain` `tracing` target with
// a digest + length only (mirrors
// [`crate::domain::idp::redact_provider_detail`]).

// @cpt-begin:cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping:p1:inst-algo-etp-domain-to-sdk
impl From<DomainError> for AccountManagementError {
    fn from(err: DomainError) -> Self {
        match err {
            // ---- Tenant CRUD ----
            DomainError::InvalidTenantType { detail } => Self::InvalidTenantType { detail },
            DomainError::Validation { detail } => Self::InvalidRequest { detail },
            DomainError::RootTenantCannotDelete => Self::RootTenantCannotDelete,
            DomainError::RootTenantCannotConvert => Self::RootTenantCannotConvert,

            DomainError::NotFound { detail, resource } => Self::TenantNotFound {
                tenant_id: resource,
                detail,
            },
            DomainError::UserNotFound { detail, resource } => Self::UserNotFound {
                user_id: resource,
                detail,
            },
            DomainError::ConversionRequestNotFound { detail, resource } => {
                Self::ConversionRequestNotFound {
                    request_id: resource,
                    detail,
                }
            }

            DomainError::AlreadyExists { detail } => Self::TenantAlreadyExists { detail },

            DomainError::TypeNotAllowed { detail } => Self::TenantTypeNotAllowed { detail },
            DomainError::TenantDepthExceeded { detail } => Self::TenantDepthExceeded { detail },
            DomainError::TenantHasChildren => Self::TenantHasChildren,
            DomainError::TenantHasResources => Self::TenantHasResources,

            // ---- Conversion request ----
            DomainError::PendingExists { request_id } => {
                Self::PendingConversionExists { request_id }
            }
            DomainError::InvalidActorForTransition {
                attempted_status,
                caller_side,
            } => Self::InvalidActorForConversionTransition {
                attempted_status,
                caller_side,
            },
            DomainError::AlreadyResolved => Self::ConversionAlreadyResolved,

            // ---- Tenant metadata ----
            DomainError::MetadataSchemaNotRegistered { detail, schema } => {
                Self::MetadataSchemaNotRegistered { schema, detail }
            }
            DomainError::MetadataEntryNotFound { detail, entry } => {
                Self::MetadataEntryNotFound { entry, detail }
            }
            DomainError::MetadataVersionMismatch {
                entry,
                expected,
                current,
            } => Self::MetadataVersionMismatch {
                entry,
                expected,
                current,
            },

            // ---- Generic precondition fallbacks ----
            DomainError::Conflict { detail } => Self::PreconditionFailed { detail },
            DomainError::FeatureDisabled { detail } => Self::FeatureDisabled { detail },

            // ---- Authorization ----
            DomainError::CrossTenantDenied { cause: _ } => Self::CrossTenantDenied,

            // ---- Transactional ----
            DomainError::Aborted { reason: _, detail } => Self::SerializationConflict { detail },

            // `IntegrityCheckInProgress` is intentionally not handled
            // here — it is not part of the inter-module SDK contract
            // (no `AccountManagementClient` method surfaces it). The
            // canonical-boundary impl below short-circuits the variant
            // straight to its 429 envelope.
            DomainError::IntegrityCheckInProgress => {
                unreachable!(
                    "DomainError::IntegrityCheckInProgress must be handled by \
                     From<DomainError> for CanonicalError before reaching the SDK boundary; \
                     no public `AccountManagementClient` method surfaces it"
                )
            }

            // ---- IdP plugin (with detail redaction) ----
            //
            // `IdpUnavailable` reuses the AIP-193 `ServiceUnavailable`
            // envelope at the canonical layer. Provider-supplied
            // `detail` can carry vendor SDK strings / endpoint names:
            // log a digest through `am.domain` and emit the variant
            // with no public detail string.
            DomainError::IdpUnavailable { detail } => {
                let (digest, len) = crate::domain::idp::redact_provider_detail(&detail);
                warn!(
                    target: "am.domain",
                    detail_digest = digest,
                    detail_len_chars = len,
                    "IdpUnavailable surfaced; provider detail redacted for log/envelope safety"
                );
                Self::IdpUnavailable
            }
            DomainError::UnsupportedOperation { detail } => {
                let (digest, len) = crate::domain::idp::redact_provider_detail(&detail);
                warn!(
                    target: "am.domain",
                    detail_digest = digest,
                    detail_len_chars = len,
                    "UnsupportedOperation surfaced; provider detail redacted for log/envelope safety"
                );
                Self::UnsupportedOperation
            }

            // ---- Generic infra ----
            //
            // `detail` is curated upstream by the adapter that
            // produced the variant — `From<EnforcerError::EvaluationFailed>`
            // emits `"authorization evaluation failed"`, the DB
            // classifier runs `redacted_db_diagnostic`, etc. Forward
            // it through so callers see the specific outage cause.
            DomainError::ServiceUnavailable {
                detail,
                retry_after,
                cause: _,
            } => Self::ServiceUnavailable {
                detail,
                retry_after_seconds: retry_after
                    .map(|d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX)),
            },

            // ---- Internal ----
            DomainError::Internal {
                diagnostic,
                cause: _,
            } => Self::Internal { detail: diagnostic },
        }
    }
}
// @cpt-end:cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping:p1:inst-algo-etp-domain-to-sdk

// ---------------------------------------------------------------------------
// AccountManagementError → CanonicalError (REST boundary).
// ---------------------------------------------------------------------------
//
// Hosted as a free `pub(crate) fn` because both `AccountManagementError`
// and `CanonicalError` are foreign to this crate — an `impl` block
// here would violate the orphan rule. Callers go through the
// `From<DomainError> for CanonicalError` impl below (REST `?`
// shorthand); direct callers (e.g. SDK clients bubbling typed errors
// to a REST adapter) call this function.

// @cpt-begin:cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping:p1:inst-algo-etp-sdk-to-canonical
/// Lift the public SDK error envelope onto the AIP-193 canonical
/// shape. Reproduces the resource-tagged builder calls AM emitted
/// before the migration to per-semantic SDK variants — same category,
/// status, resource type, field-violation / precondition-violation /
/// reason context.
#[must_use]
pub(crate) fn account_management_error_to_canonical(err: AccountManagementError) -> CanonicalError {
    use AccountManagementError as A;
    match err {
        // ---- NotFound — one resource per variant ----
        A::TenantNotFound { tenant_id, detail } => TenantResource::not_found(detail)
            .with_resource(tenant_id)
            .create(),
        A::UserNotFound { user_id, detail } => UserResource::not_found(detail)
            .with_resource(user_id)
            .create(),
        A::ConversionRequestNotFound { request_id, detail } => {
            ConversionRequestResource::not_found(detail)
                .with_resource(request_id)
                .create()
        }
        A::MetadataSchemaNotRegistered { schema, detail } => {
            TenantMetadataResource::not_found(detail)
                .with_resource(schema)
                .create()
        }
        A::MetadataEntryNotFound { entry, detail } => TenantMetadataResource::not_found(detail)
            .with_resource(entry)
            .create(),
        A::MetadataVersionMismatch {
            entry,
            expected,
            current,
        } => TenantMetadataResource::aborted(format!(
            "metadata version mismatch for {entry}: expected v{expected}, stored v{current}"
        ))
        .with_resource(entry)
        .with_reason("METADATA_VERSION_MISMATCH")
        .create(),

        // ---- InvalidArgument ----
        A::InvalidTenantType { detail } => TenantResource::invalid_argument()
            .with_field_violation("tenant_type", detail, "INVALID_TENANT_TYPE")
            .create(),
        A::InvalidRequest { detail } => TenantResource::invalid_argument()
            .with_field_violation("request", detail, "VALIDATION")
            .create(),
        A::RootTenantCannotDelete => TenantResource::invalid_argument()
            .with_field_violation(
                "tenant_id",
                "root tenant cannot be deleted",
                "ROOT_TENANT_CANNOT_DELETE",
            )
            .create(),
        A::RootTenantCannotConvert => TenantResource::invalid_argument()
            .with_field_violation(
                "tenant_id",
                "root tenant cannot be converted",
                "ROOT_TENANT_CANNOT_CONVERT",
            )
            .create(),

        // ---- AlreadyExists ----
        A::TenantAlreadyExists { detail } => TenantResource::already_exists(detail)
            .with_resource("tenant")
            .create(),

        // ---- FailedPrecondition (tenant) ----
        A::TenantTypeNotAllowed { detail } => TenantResource::failed_precondition()
            .with_precondition_violation("tenant_type", detail, "TYPE_NOT_ALLOWED")
            .create(),
        A::TenantDepthExceeded { detail } => TenantResource::failed_precondition()
            .with_precondition_violation("depth", detail, "TENANT_DEPTH_EXCEEDED")
            .create(),
        A::TenantHasChildren => TenantResource::failed_precondition()
            .with_precondition_violation(
                "tenant",
                "tenant has child tenants",
                "TENANT_HAS_CHILDREN",
            )
            .create(),
        A::TenantHasResources => TenantResource::failed_precondition()
            .with_precondition_violation(
                "tenant",
                "tenant still owns resources",
                "TENANT_HAS_RESOURCES",
            )
            .create(),
        A::PreconditionFailed { detail } => TenantResource::failed_precondition()
            .with_precondition_violation("request", detail, "PRECONDITION_FAILED")
            .create(),
        A::FeatureDisabled { detail } => TenantResource::failed_precondition()
            .with_precondition_violation("configuration", detail, "FEATURE_DISABLED")
            .create(),

        // ---- FailedPrecondition (conversion request) ----
        A::PendingConversionExists { request_id } => {
            ConversionRequestResource::failed_precondition()
                .with_precondition_violation(
                    "conversion_request",
                    format!("a pending conversion request already exists: {request_id}"),
                    "PENDING_EXISTS",
                )
                .create()
        }
        A::InvalidActorForConversionTransition {
            attempted_status,
            caller_side,
        } => ConversionRequestResource::failed_precondition()
            .with_precondition_violation(
                "conversion_request",
                format!(
                    "invalid actor for conversion transition: \
                     attempted={attempted_status} caller_side={caller_side}"
                ),
                "INVALID_ACTOR_FOR_TRANSITION",
            )
            .create(),
        A::ConversionAlreadyResolved => ConversionRequestResource::failed_precondition()
            .with_precondition_violation(
                "conversion_request",
                "conversion request already resolved",
                "ALREADY_RESOLVED",
            )
            .create(),

        // ---- Aborted (HTTP 409 with reason) ----
        A::SerializationConflict { detail } => TenantResource::aborted(detail)
            .with_reason("SERIALIZATION_CONFLICT")
            .create(),

        // ---- PermissionDenied ----
        //
        // Macro-supplied default detail: "You do not have permission
        // to perform this operation". Match the pre-migration wire
        // shape — no caller-supplied detail override.
        A::CrossTenantDenied => TenantResource::permission_denied()
            .with_reason("CROSS_TENANT_DENIED")
            .create(),

        // ---- Unimplemented ----
        A::UnsupportedOperation => {
            TenantResource::unimplemented("operation not supported by the IdP provider").create()
        }

        // ---- ServiceUnavailable ----
        //
        // `IdpUnavailable` is a tagged subset of canonical
        // `ServiceUnavailable` — the SDK enum distinguishes it for
        // typed retry logic, but the wire envelope collapses to the
        // same 503 shape (no `retry_after_seconds` because IdP retry
        // budgets are governed by the bootstrap saga, not the wire
        // hint).
        A::IdpUnavailable => CanonicalError::service_unavailable()
            .with_detail("IdP plugin unavailable")
            .create(),
        A::ServiceUnavailable {
            detail,
            retry_after_seconds,
        } => {
            let mut builder = CanonicalError::service_unavailable().with_detail(detail);
            if let Some(after) = retry_after_seconds {
                builder = builder.with_retry_after_seconds(u64::from(after));
            }
            builder.create()
        }

        // ---- Internal ----
        A::Internal { detail } => CanonicalError::internal(detail).create(),

        // `AccountManagementError` is `#[non_exhaustive]`; arms above
        // cover every variant at the time this mapping was written. A
        // future variant surfaces through this fallback as a 500 with
        // a generic diagnostic until the mapping is extended.
        #[allow(unreachable_patterns)]
        _ => CanonicalError::internal("unmapped AccountManagementError variant").create(),
    }
}
// @cpt-end:cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping:p1:inst-algo-etp-sdk-to-canonical

// ---------------------------------------------------------------------------
// DomainError → CanonicalError (REST `?` shorthand).
// ---------------------------------------------------------------------------

// @cpt-begin:cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping:p1:inst-algo-etp-domain-to-canonical
impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            // Bypass: `IntegrityCheckInProgress` is not exposed via
            // the public SDK contract (no `AccountManagementClient`
            // method surfaces it) — route directly to the canonical
            // 429 envelope without instantiating an
            // `AccountManagementError`. Wire-shape identical to the
            // pre-migration two-hop output.
            DomainError::IntegrityCheckInProgress => {
                TenantResource::resource_exhausted("integrity check already in progress")
                    .with_quota_violation(
                        "integrity_check",
                        "another integrity check is already in progress",
                    )
                    .create()
            }
            other => account_management_error_to_canonical(AccountManagementError::from(other)),
        }
    }
}
// @cpt-end:cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping:p1:inst-algo-etp-domain-to-canonical

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "sdk_error_mapping_tests.rs"]
mod sdk_error_mapping_tests;
