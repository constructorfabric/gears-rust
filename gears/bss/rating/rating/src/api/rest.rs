//! The operator REST plane under `/bss-rating/v1` (DESIGN §3.3). The foundation serves the inbox
//! release; the exception list and retry routes belong to Operations (feature 11).

pub mod dto;
pub mod handlers;

use std::sync::Arc;

use axum::http::StatusCode;
use axum::{Extension, Router};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

pub use handlers::RestState;

use crate::authz::Operation;

/// `OpenAPI` tag of every Rating route.
pub const TAG: &str = "BSS Rating";

/// The release route. DESIGN §3.3 names the AIP-136 custom method `{id}:release`, but axum 0.8
/// pins `matchit = "=0.8.4"`, which cannot split `{param}:suffix` in one segment. The route uses
/// the in-tree sub-resource fallback (account-management's `/tenants/{tenant_id}/suspend`) until
/// axum bumps matchit, so the path the router matches is the path `OpenAPI` publishes.
pub const RELEASE_PATH: &str = "/bss-rating/v1/inbox/{id}/release";

/// Registers the foundation's routes and the layers every Rating route runs under.
pub fn router(state: Arc<RestState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post(RELEASE_PATH)
        .operation_id(Operation::InboxRelease.operation_id())
        .summary("Release a quarantined inbox entry")
        .description(
            "Applies a quarantined entry through its owning feature (`apply`) or keeps it \
             unapplied for its retention (`discard`). Requires `usage:release`; audited with \
             actor, subject and request id. An entry outside the caller's scope is 404; an \
             entry that is not quarantined is 400 FailedPrecondition; releasing again to the \
             same resolution returns the entry. AIP-136 specifies the colon-method shape \
             `POST /inbox/{id}:release`, but axum 0.8.x pins `matchit = \"=0.8.4\"`, which \
             cannot split `{param}:suffix` in a single segment; this endpoint uses the in-tree \
             sub-resource fallback until axum bumps matchit.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Inbox entry id")
        .json_request::<dto::RatingInboxReleaseRequest>(openapi, "Reason and resolution")
        .handler(handlers::release_inbox_entry)
        .json_response_with_schema::<dto::RatingInboxEntryView>(
            openapi,
            StatusCode::OK,
            "The entry after the release",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(Router::new(), openapi);
    router
        .layer(Extension(state))
        .layer(axum::middleware::from_fn(
            toolkit::api::canonical_error_middleware,
        ))
}
