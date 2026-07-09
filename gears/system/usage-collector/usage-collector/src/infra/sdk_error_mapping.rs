//! Host-crate boundary lift from the flat SDK error envelope
//! [`UsageCollectorError`] onto the AIP-193 canonical
//! [`toolkit_canonical_errors::CanonicalError`], which `IntoResponse`
//! renders as the RFC-9457 `Problem` body on the REST surface. The
//! SDK-category → AIP-193-category → HTTP-status mapping is documented in
//! DESIGN §3.3 "Error Envelopes" and refreshed by ADR-0012.
//!
//! `usage-collector-sdk` deliberately carries no `toolkit-canonical-errors`
//! dependency, so the lift lives here. Both `UsageCollectorError` and
//! `CanonicalError` are foreign to this crate, so the lift is a pair of free
//! `pub(crate)` functions
//! ([`usage_collector_error_to_canonical_for_usage_type`] and
//! [`usage_collector_error_to_canonical_for_usage_record`]) rather than an
//! `impl From<…> for CanonicalError` (which would violate the orphan rule).

use toolkit_canonical_errors::{CanonicalError, Problem, resource_error};
use usage_collector_sdk::{USAGE_RECORD_RESOURCE, USAGE_TYPE_RESOURCE, UsageCollectorError};

// Resource markers — GTS resource types for the canonical envelope's
// `resource_type` field. Per ADR-0012 amendment 2026-06-08 these match
// `domain/authz.rs` exactly:
//   - UsageTypeResource → catalog REST surface (create / get / list / delete)
//   - UsageRecordResource → ingestion REST surface (create / deactivate)

#[resource_error("gts.cf.core.uc.usage_type.v1~")]
pub(crate) struct UsageTypeResource;

#[resource_error("gts.cf.core.uc.usage_record.v1~")]
pub(crate) struct UsageRecordResource;

/// Lift the SDK error envelope onto the AIP-193 canonical shape for the
/// **catalog** REST surface (`POST /usage-types`, `GET /usage-types/{gts_id}`,
/// `DELETE /usage-types/{gts_id}`, `GET /usage-types`). Use this from
/// handlers in `api/rest/handlers/usage_types.rs`. The cross-cutting
/// `PermissionDenied` variant resolves to a UsageType-shaped envelope; every
/// other variant carries its own `resource_type` and routes through
/// [`lift_common`].
// @cpt-algo:cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping:p1
#[must_use]
pub(crate) fn usage_collector_error_to_canonical_for_usage_type(
    err: UsageCollectorError,
) -> CanonicalError {
    match err {
        // @cpt-begin:cpt-cf-usage-collector-dod-usage-emission-fr-ingestion-authorization:p1:inst-dod-authz-deny
        // @cpt-begin:cpt-cf-usage-collector-dod-usage-emission-principle-fail-closed:p1:inst-dod-fail-closed-authz
        UsageCollectorError::PermissionDenied { detail } => {
            authz_denial(&detail, ResourceKind::UsageType)
        }
        // @cpt-end:cpt-cf-usage-collector-dod-usage-emission-principle-fail-closed:p1:inst-dod-fail-closed-authz
        // @cpt-end:cpt-cf-usage-collector-dod-usage-emission-fr-ingestion-authorization:p1:inst-dod-authz-deny
        other => lift_common(other),
    }
}

/// Lift the SDK error envelope onto the AIP-193 canonical shape for the
/// **ingestion** REST surface (`POST /usage-records`, `POST
/// /usage-records:batch`, `GET /usage-records`, `DELETE
/// /usage-records/{id}`, `GET /usage-records:aggregate`). Use this from
/// handlers in `api/rest/handlers/usage_records.rs`. The cross-cutting
/// `PermissionDenied` variant resolves to a UsageRecord-shaped envelope;
/// every other variant carries its own `resource_type` and routes through
/// [`lift_common`].
#[must_use]
pub(crate) fn usage_collector_error_to_canonical_for_usage_record(
    err: UsageCollectorError,
) -> CanonicalError {
    match err {
        // @cpt-begin:cpt-cf-usage-collector-dod-usage-emission-fr-ingestion-authorization:p1:inst-dod-authz-deny
        // @cpt-begin:cpt-cf-usage-collector-dod-usage-emission-principle-fail-closed:p1:inst-dod-fail-closed-authz
        UsageCollectorError::PermissionDenied { detail } => {
            authz_denial(&detail, ResourceKind::UsageRecord)
        }
        // @cpt-end:cpt-cf-usage-collector-dod-usage-emission-principle-fail-closed:p1:inst-dod-fail-closed-authz
        // @cpt-end:cpt-cf-usage-collector-dod-usage-emission-fr-ingestion-authorization:p1:inst-dod-authz-deny
        other => lift_common(other),
    }
}

/// Fallback for a `resource_type` that is neither [`USAGE_TYPE_RESOURCE`]
/// nor [`USAGE_RECORD_RESOURCE`]. The flat SDK contract only ever sets those
/// two, so an unrecognized value is a host-side breach — assert in debug,
/// surface a redacted `internal` on the wire rather than mislabel a resource.
fn unrecognized_resource(resource_type: &str) -> CanonicalError {
    debug_assert!(
        false,
        "unrecognized resource_type on SDK error: {resource_type}"
    );
    CanonicalError::internal(format!("unrecognized resource_type: {resource_type}")).create()
}

/// Surface-independent lift for every non-`PermissionDenied` category. The
/// `resource_type` carried on the variant selects the GTS resource marker;
/// the typed `reason` / `field` ride straight onto the canonical envelope
/// (`field_violations[0].reason` for 400s, `context.reason` for 409
/// `Aborted`s). 503 `ServiceUnavailable` carries no `context.reason` — the
/// canonical `ServiceUnavailable` context has no such slot, so operator
/// triage reads the curated `detail` string.
fn lift_common(err: UsageCollectorError) -> CanonicalError {
    use UsageCollectorError as E;
    match err {
        // ---- 400 InvalidArgument ----
        // `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`
        // (counter/gauge matrix), the validating-newtype rejects, the
        // metadata-shape / size checks, and the bad-prefix gts_id parse all
        // route here; `field` + typed `reason` carry the discriminator and
        // `resource_type` names the violated resource (a `gts_id`-shaped
        // violation attributes to the usage type even on the ingestion
        // surface).
        E::InvalidArgument {
            resource_type,
            resource_name,
            field,
            reason,
            detail,
        } => {
            let wire_reason = reason.as_wire();
            if resource_type == USAGE_TYPE_RESOURCE {
                let b = UsageTypeResource::invalid_argument().with_field_violation(
                    field,
                    detail,
                    wire_reason,
                );
                match resource_name {
                    Some(name) => b.with_resource(name).create(),
                    None => b.create(),
                }
            } else if resource_type == USAGE_RECORD_RESOURCE {
                let b = UsageRecordResource::invalid_argument().with_field_violation(
                    field,
                    detail,
                    wire_reason,
                );
                match resource_name {
                    Some(name) => b.with_resource(name).create(),
                    None => b.create(),
                }
            } else {
                unrecognized_resource(&resource_type)
            }
        }

        // ---- 404 NotFound ----
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-not-found
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-not-found
        // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping:p1:inst-algo-outcome-not-found
        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-not-found
        E::NotFound {
            resource_type,
            name,
            detail,
        } => {
            if resource_type == USAGE_TYPE_RESOURCE {
                UsageTypeResource::not_found(detail)
                    .with_resource(name)
                    .create()
            } else if resource_type == USAGE_RECORD_RESOURCE {
                UsageRecordResource::not_found(detail)
                    .with_resource(name)
                    .create()
            } else {
                unrecognized_resource(&resource_type)
            }
        }
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-not-found
        // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping:p1:inst-algo-outcome-not-found
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-not-found
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-not-found

        // ---- 409 AlreadyExists ----
        E::AlreadyExists {
            resource_type,
            name,
            detail,
        } => {
            if resource_type == USAGE_TYPE_RESOURCE {
                UsageTypeResource::already_exists(detail)
                    .with_resource(name)
                    .create()
            } else if resource_type == USAGE_RECORD_RESOURCE {
                UsageRecordResource::already_exists(detail)
                    .with_resource(name)
                    .create()
            } else {
                unrecognized_resource(&resource_type)
            }
        }

        // ---- 409 Aborted (Conflict) ----
        // Referential-integrity on delete (`USAGE_TYPE_REFERENCED`), the
        // already-inactive deactivation latch, the idempotency conflict, and
        // the L1 `corrects_id` rules all collapse here; the typed
        // `ConflictReason` rides on `context.reason`.
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-referenced
        // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-idempotency:p1
        // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-principle-idempotency-by-key:p1
        // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping:p1:inst-algo-outcome-already-inactive
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-validate-fail
        E::Conflict {
            resource_type,
            name,
            reason,
            detail,
        } => {
            let wire_reason = reason.as_wire();
            if resource_type == USAGE_TYPE_RESOURCE {
                UsageTypeResource::aborted(detail)
                    .with_resource(name)
                    .with_reason(wire_reason)
                    .create()
            } else if resource_type == USAGE_RECORD_RESOURCE {
                UsageRecordResource::aborted(detail)
                    .with_resource(name)
                    .with_reason(wire_reason)
                    .create()
            } else {
                unrecognized_resource(&resource_type)
            }
        }
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-validate-fail
        // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping:p1:inst-algo-outcome-already-inactive
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-referenced

        // ---- 503 ServiceUnavailable (surface-less) ----
        // @cpt-begin:cpt-cf-usage-collector-dod-usage-emission-principle-pluggable-storage:p1:inst-dod-pluggable-storage-fail
        E::ServiceUnavailable {
            retry_after_seconds,
            detail,
        } => {
            let mut builder = CanonicalError::service_unavailable().with_detail(detail);
            if let Some(after) = retry_after_seconds {
                builder = builder.with_retry_after_seconds(after);
            }
            builder.create()
        }
        // @cpt-end:cpt-cf-usage-collector-dod-usage-emission-principle-pluggable-storage:p1:inst-dod-pluggable-storage-fail

        // ---- 500 Internal ----
        // `detail` is DSN-free and pre-redacted at the construction site by
        // the flat SDK error contract; carried as the internal diagnostic,
        // never leaked to the public wire body.
        E::Internal { detail } => CanonicalError::internal(detail).create(),

        // `UsageCollectorError` is `#[non_exhaustive]`, and `PermissionDenied`
        // is handled by the surface entry points; fail closed to a generic
        // 500 rather than leak an unmapped wire shape.
        other => {
            debug_assert!(false, "lift_common missing arm for variant: {other:?}");
            CanonicalError::internal("unmapped usage-collector error variant").create()
        }
    }
}

/// The two REST surfaces share a closed set of GTS resources — picked at the
/// call site by which lift was invoked, used only for the cross-cutting
/// `PermissionDenied` envelope.
#[derive(Clone, Copy)]
enum ResourceKind {
    UsageType,
    UsageRecord,
}

/// PDP denial: same envelope shape on both surfaces; only the
/// `resource_type` changes. PDP-supplied detail is intentionally dropped
/// from the wire envelope (never paraphrased to callers), but kept in
/// operator logs so denial triage doesn't collapse to a bare "AUTHZ".
fn authz_denial(detail: &str, kind: ResourceKind) -> CanonicalError {
    tracing::warn!(deny_reason = %detail, "PDP denied request");
    match kind {
        ResourceKind::UsageType => UsageTypeResource::permission_denied(),
        ResourceKind::UsageRecord => UsageRecordResource::permission_denied(),
    }
    .with_reason("AUTHZ")
    .create()
}

/// Lift a per-record [`UsageCollectorError`] onto an RFC-9457 [`Problem`] for
/// the ingestion REST surface. Used by the batch handler in
/// [`crate::api::rest::handlers::usage_records`]; whole-request rejections go
/// through [`usage_collector_error_to_canonical_for_usage_record`] directly
/// via the normal `IntoResponse` path. Both paths now share the same lift, so
/// a per-record and a whole-request rejection of the same error are
/// byte-identical on the wire.
#[must_use]
pub(crate) fn usage_record_error_to_problem(err: UsageCollectorError) -> Problem {
    Problem::from(usage_collector_error_to_canonical_for_usage_record(err))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "sdk_error_mapping_tests.rs"]
mod sdk_error_mapping_tests;
