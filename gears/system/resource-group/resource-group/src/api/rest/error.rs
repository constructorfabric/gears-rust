// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-05-07 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-sdk-foundation-sdk-errors:p1:inst-full
// @cpt-algo:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1
//! Map domain errors to canonical errors (`toolkit-canonical-errors`) for
//! REST responses. Handlers return `ApiResult<T>` (= `Result<T,
//! CanonicalError>`); the canonical error middleware
//! (`toolkit::api::canonical_error_middleware`) converts the `CanonicalError`
//! to a wire `Problem` and fills `instance` / `trace_id` post-response.

use resource_group_sdk::{field, precondition, reason};
use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

/// Errors attributable to a resource group as a resource.
///
/// The macro literal mirrors [`resource_group_sdk::gts::GROUP_RESOURCE_TYPE`]
/// (proc-macros cannot resolve a const); the SDK round-trip tests pin the
/// two equal.
#[resource_error("gts.cf.core.resource_group.group.v1~")]
pub struct RgError;

/// Implement `From<DomainError> for CanonicalError` so `?` works in
/// handlers that return `ApiResult<T>`.
impl From<DomainError> for CanonicalError {
    #[allow(clippy::cognitive_complexity)]
    fn from(e: DomainError) -> Self {
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-1
        // Receive DomainError variant
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-1
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-3
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-4
        match e {
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2a
            DomainError::Validation { message } => {
                RgError::invalid_argument().with_format(message).create()
            }
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2a
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2b
            DomainError::TypeNotFound { code } => {
                RgError::not_found(format!("GTS type with code '{code}' was not found"))
                    .with_resource(code)
                    .create()
            }
            DomainError::GroupNotFound { id } => {
                RgError::not_found(format!("Resource group with id '{id}' was not found"))
                    .with_resource(id.to_string())
                    .create()
            }
            DomainError::MembershipNotFound { key } => {
                RgError::not_found(format!("Membership '{key}' was not found"))
                    .with_resource(key)
                    .create()
            }
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2b
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2c
            DomainError::TypeAlreadyExists { code } => {
                RgError::already_exists(format!("GTS type with code '{code}' already exists"))
                    .with_resource(code)
                    .create()
            }
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2c
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2d
            DomainError::InvalidParentType { message } => RgError::invalid_argument()
                .with_field_violation(
                    field::PARENT_TYPE_FIELD,
                    message,
                    field::INVALID_PARENT_TYPE,
                )
                .create(),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2d
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2e
            // ⚠ wire change accepted in the migration plan: 409 → 400.
            DomainError::AllowedParentTypesViolation { message } => RgError::failed_precondition()
                .with_precondition_violation(
                    precondition::ALLOWED_PARENTS_SUBJECT,
                    message,
                    precondition::STATE_TYPE,
                )
                .create(),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2e
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2f
            // ⚠ wire change accepted in the migration plan: 409 → 400.
            DomainError::CycleDetected { message } => RgError::failed_precondition()
                .with_precondition_violation(
                    precondition::HIERARCHY_SUBJECT,
                    message,
                    precondition::STATE_TYPE,
                )
                .create(),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2f
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2g
            // ⚠ wire change accepted in the migration plan: 409 → 400.
            DomainError::ConflictActiveReferences { message } => RgError::failed_precondition()
                .with_precondition_violation(
                    precondition::ACTIVE_REFERENCES_SUBJECT,
                    message,
                    precondition::STATE_TYPE,
                )
                .create(),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2g
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2h
            // ⚠ wire change accepted in the migration plan: 409 → 400.
            DomainError::LimitViolation { message } => RgError::failed_precondition()
                .with_precondition_violation(
                    precondition::LIMIT_SUBJECT,
                    message,
                    precondition::STATE_TYPE,
                )
                .create(),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2h
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2i
            // ⚠ wire change accepted in the migration plan: 409 → 400.
            DomainError::TenantIncompatibility { message } => RgError::failed_precondition()
                .with_precondition_violation(
                    precondition::TENANT_SUBJECT,
                    message,
                    precondition::STATE_TYPE,
                )
                .create(),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2i
            // Duplicate-on-create variants route through `already_exists`
            // with the structural resource id as `resource_name` (matches
            // the spec semantic for duplicate-on-create — see
            // `docs/arch/errors/categories/06-already-exists.md`).
            DomainError::DuplicateMembership { key, message } => {
                RgError::already_exists(message).with_resource(key).create()
            }
            DomainError::TenantRootAlreadyExists {
                existing_root_id,
                detail,
            } => RgError::already_exists(detail)
                .with_resource(existing_root_id.to_string())
                .create(),
            // Generic conflict carries no structural resource id — route
            // through `aborted` with a stable reason discriminator.
            DomainError::Conflict { message } => RgError::aborted(message)
                .with_reason(reason::aborted::CONFLICT)
                .create(),
            DomainError::AccessDenied { message } => {
                tracing::debug!(reason = %message, "resource-group access denied");
                RgError::permission_denied()
                    .with_reason(reason::permission::ACCESS_DENIED)
                    .create()
            }
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2j
            // ServiceUnavailable: no dedicated variant — DB / infra failures
            // fall through to the Database arm below and surface as a
            // canonical Internal (HTTP 500). A genuine 503 (e.g. AuthZ
            // Resolver unreachable) is produced by platform middleware
            // upstream of this mapper, not here.
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2j
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2k
            // Source description flows into the canonical's `ctx.description`
            // (recoverable via `diagnostic()`) so `canonical_error_middleware`
            // (DESIGN.md §3.6) logs it server-side with the request `trace_id`.
            // `Internal::description` is `#[serde(skip)]`, so the DB text
            // never reaches the wire `detail`.
            DomainError::Database(db_err) => {
                CanonicalError::internal(format!("resource-group DB error: {db_err}")).create()
            }
            DomainError::InternalError => {
                CanonicalError::internal("resource-group internal error").create()
            } // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2k
        }
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-4
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-3
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2
    }
}
// @cpt-end:cpt-cf-resource-group-dod-sdk-foundation-sdk-errors:p1:inst-full
