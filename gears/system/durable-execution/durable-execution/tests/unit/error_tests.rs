use super::*;
use durable_execution_sdk::{DefinitionError, RunId, gts};
use toolkit_canonical_errors::context::InvalidArgument;
use uuid::Uuid;

fn canonical(e: DomainError) -> CanonicalError {
    CanonicalError::from(e)
}

#[test]
fn resource_markers_match_sdk_gts_constants() {
    assert_eq!(
        canonical(DomainError::RunNotFound(RunId(Uuid::nil()))).resource_type(),
        Some(gts::RUN_RESOURCE_TYPE)
    );
    assert_eq!(
        canonical(DomainError::DefinitionNotFound("demo.v1".into())).resource_type(),
        Some(gts::DEFINITION_RESOURCE_TYPE)
    );
}

#[test]
fn forbidden_is_permission_denied() {
    for (domain, resource) in [
        (DomainError::Forbidden, gts::RUN_RESOURCE_TYPE),
        (
            DomainError::DefinitionForbidden,
            gts::DEFINITION_RESOURCE_TYPE,
        ),
    ] {
        let err = canonical(domain);
        assert_eq!(err.status_code(), 403);
        assert_eq!(err.resource_type(), Some(resource));
        assert!(
            matches!(&err, CanonicalError::PermissionDenied { ctx, .. } if ctx.reason == reason::EXECUTION_ACCESS_DENIED)
        );
    }
}

#[test]
fn not_found_names_the_resource() {
    let id = RunId(Uuid::new_v4());
    let err = canonical(DomainError::RunNotFound(id));
    assert_eq!(err.status_code(), 404);
    assert_eq!(err.resource_name(), Some(id.0.to_string().as_str()));

    let err = canonical(DomainError::DefinitionNotFound("demo.v1".into()));
    assert!(matches!(err, CanonicalError::NotFound { .. }));
    assert_eq!(err.resource_name(), Some("demo.v1"));
}

#[test]
fn races_are_aborted_with_reason() {
    for (domain, expected, resource) in [
        (
            DomainError::LeaseLost,
            reason::LEASE_LOST,
            gts::RUN_RESOURCE_TYPE,
        ),
        (
            DomainError::ConcurrentUpdate,
            reason::CONCURRENT_UPDATE,
            gts::RUN_RESOURCE_TYPE,
        ),
        (
            DomainError::DefinitionConcurrentUpdate,
            reason::CONCURRENT_UPDATE,
            gts::DEFINITION_RESOURCE_TYPE,
        ),
    ] {
        let err = canonical(domain);
        assert_eq!(err.status_code(), 409);
        assert_eq!(err.resource_type(), Some(resource));
        assert!(matches!(&err, CanonicalError::Aborted { ctx, .. } if ctx.reason == expected));
    }
}

#[test]
fn preconditions_carry_violation_type() {
    for (domain, expected, resource) in [
        (
            DomainError::InvalidState("run is still executing"),
            reason::INVALID_STATE,
            gts::RUN_RESOURCE_TYPE,
        ),
        (
            DomainError::DefinitionInvalidState("definition is stopping"),
            reason::INVALID_STATE,
            gts::DEFINITION_RESOURCE_TYPE,
        ),
        (
            DomainError::DefinitionInactive,
            reason::DEFINITION_INACTIVE,
            gts::DEFINITION_RESOURCE_TYPE,
        ),
        (
            DomainError::DefinitionMismatch,
            reason::DEFINITION_MISMATCH,
            gts::DEFINITION_RESOURCE_TYPE,
        ),
    ] {
        let err = canonical(domain);
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.resource_type(), Some(resource));
        match err {
            CanonicalError::FailedPrecondition { ctx, .. } => {
                assert_eq!(ctx.violations.len(), 1);
                assert_eq!(ctx.violations[0].type_, expected);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn conflicting_content_is_already_exists() {
    let err = canonical(DomainError::DefinitionConflict("demo.v1".into()));
    assert_eq!(err.status_code(), 409);
    assert!(matches!(err, CanonicalError::AlreadyExists { .. }));
    assert_eq!(err.resource_type(), Some(gts::DEFINITION_RESOURCE_TYPE));
    assert_eq!(err.resource_name(), Some("demo.v1"));

    let err = canonical(DomainError::IdempotencyConflict("idempotency_key"));
    assert!(matches!(err, CanonicalError::AlreadyExists { .. }));
    assert_eq!(err.resource_type(), Some(gts::RUN_RESOURCE_TYPE));
    assert_eq!(err.resource_name(), Some("idempotency_key"));
}

#[test]
fn invalid_input_is_invalid_argument() {
    let err = canonical(DomainError::InvalidRequest {
        field: "input",
        reason: reason::PAYLOAD_TOO_LARGE,
        message: "input exceeds 256 KiB",
    });
    assert_eq!(err.status_code(), 400);
    match err {
        CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { field_violations },
            ..
        } => {
            assert_eq!(field_violations[0].field, "input");
            assert_eq!(field_violations[0].reason, reason::PAYLOAD_TOO_LARGE);
        }
        other => panic!("unexpected {other:?}"),
    }

    let err = canonical(DefinitionError::new("invalid retry policy").into());
    assert!(matches!(
        err,
        CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { .. },
            ..
        }
    ));
}

#[test]
fn infrastructure_failures() {
    let err = canonical(DomainError::Unavailable);
    assert_eq!(err.status_code(), 503);
    assert!(matches!(err, CanonicalError::ServiceUnavailable { .. }));

    let err = canonical(DomainError::Internal("fence overflow"));
    assert_eq!(err.status_code(), 500);
    assert!(matches!(err, CanonicalError::Internal { .. }));
}
