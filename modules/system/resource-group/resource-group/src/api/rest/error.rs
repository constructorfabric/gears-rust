// @cpt-begin:cpt-cf-resource-group-dod-sdk-foundation-sdk-errors:p1:inst-full
// @cpt-algo:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1
//! Map domain errors to RFC 9457 Problem Details for REST responses.

use modkit::api::problem::Problem;

use crate::domain::error::DomainError;

/// Implement `Into<Problem>` for `DomainError` so `?` works in handlers.
impl From<DomainError> for Problem {
    fn from(e: DomainError) -> Self {
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-1
        // Receive DomainError variant
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-1
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-3
        #[allow(clippy::let_and_return)]
        let problem = match &e {
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2a
            DomainError::Validation { message } => {
                Problem::new(http::StatusCode::BAD_REQUEST, "Validation error", message)
            }
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2a
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2b
            DomainError::TypeNotFound { code } => Problem::new(
                http::StatusCode::NOT_FOUND,
                "Type not found",
                format!("GTS type with code '{code}' was not found"),
            ),
            DomainError::GroupNotFound { id } => Problem::new(
                http::StatusCode::NOT_FOUND,
                "Group not found",
                format!("Resource group with id '{id}' was not found"),
            ),
            DomainError::MembershipNotFound { key } => Problem::new(
                http::StatusCode::NOT_FOUND,
                "Membership not found",
                format!("Membership '{key}' was not found"),
            ),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2b
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2c
            DomainError::TypeAlreadyExists { code } => Problem::new(
                http::StatusCode::CONFLICT,
                "Type already exists",
                format!("GTS type with code '{code}' already exists"),
            ),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2c
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2d
            DomainError::InvalidParentType { message } => Problem::new(
                http::StatusCode::BAD_REQUEST,
                "Invalid parent type",
                message,
            ),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2d
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2e
            DomainError::AllowedParentsViolation { message } => Problem::new(
                http::StatusCode::CONFLICT,
                "Allowed parents violation",
                message,
            ),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2e
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2f
            DomainError::CycleDetected { message } => {
                Problem::new(http::StatusCode::CONFLICT, "Cycle detected", message)
            }
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2f
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2g
            DomainError::ConflictActiveReferences { message } => Problem::new(
                http::StatusCode::CONFLICT,
                "Active references exist",
                message,
            ),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2g
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2h
            DomainError::LimitViolation { message } => {
                Problem::new(http::StatusCode::CONFLICT, "Limit violation", message)
            }
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2h
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2i
            DomainError::TenantIncompatibility { message } => Problem::new(
                http::StatusCode::CONFLICT,
                "Tenant incompatibility",
                message,
            ),
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2i
            DomainError::Conflict { message } => {
                Problem::new(http::StatusCode::CONFLICT, "Conflict", message)
            }
            DomainError::AccessDenied { message } => {
                Problem::new(http::StatusCode::FORBIDDEN, "Access denied", message)
            }
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2j
            // ServiceUnavailable -> 503 (no dedicated variant; covered by Database fallback)
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2j
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2k
            DomainError::Database { .. } => {
                tracing::error!(error = ?e, "Database error occurred");
                Problem::new(
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal error",
                    "An internal database error occurred",
                )
            }
            DomainError::InternalError => {
                tracing::error!(error = ?e, "Internal error occurred");
                Problem::new(
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal error",
                    "An internal error occurred",
                )
            } // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2k
        };
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-3
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-2
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-4
        problem
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1:inst-err-map-4
    }
}
// @cpt-end:cpt-cf-resource-group-dod-sdk-foundation-sdk-errors:p1:inst-full
