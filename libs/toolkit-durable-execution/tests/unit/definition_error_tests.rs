use super::*;
use crate::Resource;

#[test]
fn definition_error_display_names_the_rule_and_step() {
    let error = DefinitionError::new(DefinitionErrorKind::DuplicateActivityId).for_step("charge");
    assert_eq!(error.kind(), DefinitionErrorKind::DuplicateActivityId);
    assert_eq!(error.step(), Some("charge"));
    assert_eq!(
        error.to_string(),
        "invalid execution definition: duplicate activity id (step `charge`)"
    );
    assert_eq!(
        DefinitionError::new(DefinitionErrorKind::Encoding).to_string(),
        "invalid execution definition: contract cannot be encoded"
    );
}

#[test]
fn errors_expose_kind_code_and_structured_details() {
    use crate::error::domain::DomainError;
    let run = RunId(uuid::Uuid::from_u128(7));
    let cases = [
        (
            DomainError::Forbidden,
            ErrorKind::PermissionDenied,
            "EXECUTION_ACCESS_DENIED",
        ),
        (
            DomainError::DefinitionForbidden,
            ErrorKind::PermissionDenied,
            "EXECUTION_ACCESS_DENIED",
        ),
        (
            DomainError::RunNotFound(run),
            ErrorKind::NotFound,
            "RUN_NOT_FOUND",
        ),
        (
            DomainError::DefinitionNotFound("a.v1".into()),
            ErrorKind::NotFound,
            "DEFINITION_NOT_FOUND",
        ),
        (DomainError::LeaseLost, ErrorKind::Aborted, "LEASE_LOST"),
        (
            DomainError::ConcurrentUpdate,
            ErrorKind::Aborted,
            "CONCURRENT_UPDATE",
        ),
        (
            DomainError::DefinitionConcurrentUpdate,
            ErrorKind::Aborted,
            "CONCURRENT_UPDATE",
        ),
        (
            DomainError::InvalidState("x"),
            ErrorKind::FailedPrecondition,
            "INVALID_STATE",
        ),
        (
            DomainError::DefinitionInvalidState("x"),
            ErrorKind::FailedPrecondition,
            "INVALID_STATE",
        ),
        (
            DomainError::DefinitionInactive,
            ErrorKind::FailedPrecondition,
            "DEFINITION_INACTIVE",
        ),
        (
            DomainError::DefinitionMismatch,
            ErrorKind::FailedPrecondition,
            "DEFINITION_MISMATCH",
        ),
        (
            DomainError::DefinitionConflict("a.v1".into()),
            ErrorKind::AlreadyExists,
            "DEFINITION_CONFLICT",
        ),
        (
            DomainError::IdempotencyConflict("idempotency_key"),
            ErrorKind::AlreadyExists,
            "REQUEST_KEY_CONFLICT",
        ),
        (
            DomainError::InvalidDefinition(DefinitionError::new(DefinitionErrorKind::Encoding)),
            ErrorKind::InvalidArgument,
            "INVALID_DEFINITION",
        ),
        (
            DomainError::InvalidRequest {
                field: "input",
                reason: codes::PAYLOAD_TOO_LARGE,
                message: "too large",
            },
            ErrorKind::InvalidArgument,
            "PAYLOAD_TOO_LARGE",
        ),
        (
            DomainError::Unavailable,
            ErrorKind::Unavailable,
            "UNAVAILABLE",
        ),
        (DomainError::Internal("x"), ErrorKind::Internal, "INTERNAL"),
        (
            DomainError::DeliveryDisabled,
            ErrorKind::FailedPrecondition,
            "DELIVERY_DISABLED",
        ),
    ];
    for (domain, kind, code) in cases {
        let error = Error::from(domain);
        assert_eq!(error.kind(), kind, "{error}");
        assert_eq!(error.code(), code, "{error}");
        assert_eq!(
            error.is_retryable(),
            matches!(kind, ErrorKind::Aborted | ErrorKind::Unavailable)
        );
    }
    assert_eq!(
        Error::from(DomainError::RunNotFound(run)).run_id(),
        Some(run)
    );
    assert_eq!(
        Error::from(DomainError::DefinitionConflict("a.v1".into())).definition(),
        Some("a.v1")
    );
    assert_eq!(
        Error::from(DomainError::IdempotencyConflict("coalescing_key")).request_key(),
        Some("coalescing_key")
    );
    assert_eq!(
        Error::invalid_input("input", "bad").field().as_deref(),
        Some("input")
    );
    let step =
        Error::from(DefinitionError::new(DefinitionErrorKind::InvalidTimeout).for_step("charge"));
    assert_eq!(step.field().as_deref(), Some("activities.charge"));
    let decode = serde_json::from_str::<u32>("\"x\"").unwrap_err();
    let undecodable = Error::undecodable(true, decode);
    assert_eq!(undecodable.kind(), ErrorKind::Internal);
    assert!(undecodable.to_string().contains("checkpoint"));
    assert!(undecodable.source().is_some());
}

#[test]
fn errors_name_the_resource_they_concern() {
    use crate::error::domain::DomainError;
    let run = Some(Resource::Run);
    let definition = Some(Resource::Definition);
    // Every variant: a misplaced arm would map a run failure onto the catalog.
    for (domain, resource) in [
        (DomainError::Forbidden, run),
        (DomainError::RunNotFound(RunId(uuid::Uuid::nil())), run),
        (DomainError::LeaseLost, run),
        (DomainError::ConcurrentUpdate, run),
        (DomainError::InvalidState("x"), run),
        (DomainError::IdempotencyConflict("idempotency_key"), run),
        (
            DomainError::InvalidRequest {
                field: "input",
                reason: codes::INVALID_FORMAT,
                message: "x",
            },
            run,
        ),
        (DomainError::DefinitionForbidden, definition),
        (DomainError::DefinitionNotFound("a.v1".into()), definition),
        (DomainError::DefinitionConcurrentUpdate, definition),
        (DomainError::DefinitionInvalidState("x"), definition),
        (DomainError::DefinitionConflict("a.v1".into()), definition),
        (DomainError::DefinitionMismatch, definition),
        (DomainError::DefinitionInactive, definition),
        (
            DomainError::InvalidDefinition(DefinitionError::new(DefinitionErrorKind::Encoding)),
            definition,
        ),
        (DomainError::DeliveryDisabled, None),
        (DomainError::Unavailable, None),
        (DomainError::Internal("x"), None),
    ] {
        assert_eq!(Error::from(domain.clone()).resource(), resource, "{domain}");
    }
}

#[test]
fn store_failures_keep_their_cause_and_equality_ignores_it() {
    let store = StoreError::Database(toolkit_db::DbError::from(sea_orm::DbErr::Custom(
        "fixture database unavailable".into(),
    )));
    let error = Error::from(store);
    assert_eq!(error.kind(), ErrorKind::Unavailable);
    assert!(error.source().is_some());
    assert_eq!(
        error,
        Error::from(crate::error::domain::DomainError::Unavailable)
    );
    let disabled = Error::from(StoreError::DeliveryDisabled);
    assert_eq!(disabled.code(), codes::DELIVERY_DISABLED);
    assert!(!disabled.is_retryable());
    // Configured delivery that has not started yet is a transient outage.
    let pending = Error::from(StoreError::DeliveryUnavailable);
    assert_eq!(pending.kind(), ErrorKind::Unavailable);
    assert!(pending.is_retryable());
    let denied = Error::from(StoreError::Authorization(
        crate::AuthorizationError::denied(),
    ));
    assert_eq!(denied.kind(), ErrorKind::PermissionDenied);
    assert!(denied.source().is_some());
    assert!(!format!("{error:?}").contains("fixture database"));
}
