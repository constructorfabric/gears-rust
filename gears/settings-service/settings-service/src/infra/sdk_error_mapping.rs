// Created: 2026-08-12 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-gear-foundation-problem-mapping:p1
//! The single conversion from [`DomainError`] to the platform error type.
//!
//! One arm per variant, deliberately flat: a reviewer checks this against the
//! enum by eye, and any nesting would hide an unmapped variant. Everything the
//! problem document needs — the `gts://` type URI, `title`, `status`, `trace_id`
//! — is derived by the platform from the canonical category chosen here, so the
//! only decision this file makes is *which category*, and that decision is what
//! the caller sees as an HTTP status.

use toolkit_canonical_errors::{CanonicalError, Http, resource_error};

use crate::domain::error::DomainError;
use crate::precondition;

/// The resource this gear attributes its errors to.
#[resource_error("gts.cf.toolkit.settings.declaration.v1~")]
struct SettingsResource;

/// Errors attributed to a category.
#[resource_error("gts.cf.toolkit.settings.category.v1~")]
struct CategoryResource;

/// Errors attributed to a stored setting value.
#[resource_error("gts.cf.toolkit.settings.value.v1~")]
struct ValueResource;

/// Build a denial attributed to the resource actually enforced.
///
/// The `#[resource_error]` macro fixes one type per scope, so a denial has to
/// select the scope matching what the caller asked for — otherwise a category
/// request is refused with the declaration type in its context, which is simply
/// wrong metadata for anyone reading a log or an audit trail.
fn permission_denied_for(resource: &'static str) -> CanonicalError {
    const REASON: &str = "SETTING_NOT_ENTITLED";
    if resource == settings_service_sdk::gts::CATEGORY_SCHEMA {
        CategoryResource::permission_denied()
            .with_reason(REASON)
            .create()
    } else if resource == settings_service_sdk::gts::VALUE_SCHEMA {
        ValueResource::permission_denied()
            .with_reason(REASON)
            .create()
    } else {
        SettingsResource::permission_denied()
            .with_reason(REASON)
            .create()
    }
}

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-problem-mapping:p1:inst-gf-problem-1
        match err {
            // 422 — the only path that carries field-level detail.
            DomainError::Validation {
                field,
                code,
                message,
            } => {
                // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-problem-mapping:p1:inst-gf-problem-4
                SettingsResource::invalid_argument()
                    .with_field_violation(field, message, code)
                    .create()
                // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-problem-mapping:p1:inst-gf-problem-4
            }

            // 428 — no `If-Match` at all. RFC 9110 reserves this status for
            // exactly this case; without the override it would render as 400
            // and be indistinguishable from a malformed body.
            DomainError::PreconditionRequired { detail } => SettingsResource::invalid_argument()
                .with_field_violation(
                    crate::api::precondition::IF_MATCH_HEADER,
                    detail,
                    crate::field::IF_MATCH_REQUIRED,
                )
                .with_override(Http::status_code(428))
                .create(),

            // 412 — a conditional write whose precondition no longer holds.
            //
            // The canonical model has no precondition-failed status: the
            // category defaults to 400, which a caller cannot distinguish from
            // a malformed body. RFC 9110 gives conditional requests 412, and
            // clients retry on it by re-reading and re-sending, so the status
            // is overridden explicitly. The category is unchanged — the
            // override only moves the status within the same 4xx class.
            DomainError::PreconditionFailed { detail } => SettingsResource::failed_precondition()
                .with_precondition_violation("setting", detail, precondition::ETAG_MISMATCH)
                .with_override(Http::status_code(412))
                .create(),

            // 409 — the request conflicts with current state.
            DomainError::Conflict { detail } => SettingsResource::already_exists(detail)
                .with_resource("setting")
                .create(),

            // 403 — carries nothing about the target, by construction: the
            // variant holds no identifier to leak, so a denial for a setting
            // that exists is byte-identical to one for a setting that does not.
            // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-problem-mapping:p1:inst-gf-problem-5
            DomainError::Unauthorized { resource } => permission_denied_for(resource),
            // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-problem-mapping:p1:inst-gf-problem-5

            // 404 — names the kind of resource, never the caller's identifier.
            DomainError::NotFound { resource } => {
                SettingsResource::not_found(format!("no such {resource}"))
                    .with_resource(resource)
                    .create()
            }

            // 503 — this service does not mask its own unavailability, so the
            // caller can retry or degrade on its own terms.
            DomainError::Unavailable { detail } => CanonicalError::service_unavailable()
                .with_detail(detail)
                .create(),

            // 500 — the diagnostic goes to the in-process channel the platform
            // strips from the wire, so nothing internal reaches the caller.
            // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-problem-mapping:p1:inst-gf-problem-6
            DomainError::Internal { diagnostic } => CanonicalError::internal(diagnostic).create(),
            // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-problem-mapping:p1:inst-gf-problem-6
        }
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-problem-mapping:p1:inst-gf-problem-1
    }
}

#[cfg(test)]
#[path = "sdk_error_mapping_tests.rs"]
mod sdk_error_mapping_tests;
