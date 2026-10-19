use bss_rating_sdk::errors::reason;
use serde_json::Value;
use toolkit_canonical_errors::{CanonicalError, Problem};
use uuid::Uuid;

use super::RETRY_AFTER_SECONDS;
use crate::authz::{Operation, labels};
use crate::domain::error::{Cause, DomainError};
use crate::domain::inbox::InboxSource;

/// The status and the problem's `context`, as a caller receives them.
fn wire(err: DomainError) -> (u16, Value) {
    let problem = Problem::from(CanonicalError::from(err));
    (problem.status.unwrap_or_default(), problem.context)
}

#[test]
fn a_refusal_about_the_target_names_its_reason() {
    let (status, context) = wire(DomainError::NotFound {
        what: "inbox entry",
    });
    assert_eq!(status, 404);
    assert_eq!(context["resource_name"], "inbox entry", "{context}");

    let (status, context) = wire(DomainError::NotQuarantined {
        entry_id: Uuid::nil(),
    });
    assert_eq!(status, 400);
    assert_eq!(
        context["violations"][0]["type"],
        reason::INBOX_ENTRY_NOT_QUARANTINED
    );

    let (status, context) = wire(DomainError::SourceNotApplicable {
        inbox_source: InboxSource::Fact,
    });
    assert_eq!(status, 400);
    assert_eq!(
        context["violations"][0]["type"],
        reason::INBOX_SOURCE_NOT_APPLICABLE
    );
    assert_eq!(context["violations"][0]["subject"], "fact");
}

#[test]
fn every_denial_is_permission_denied() {
    for denied in [
        DomainError::ProducerNotVerified {
            inbox_source: InboxSource::Fact,
        },
        DomainError::OutsideGrant {
            tenant_id: Uuid::nil(),
        },
        DomainError::PermissionDenied("x".to_owned()),
    ] {
        let (status, context) = wire(denied);
        assert_eq!(status, 403);
        assert_eq!(context["reason"], reason::PERMISSION_DENIED);
    }
}

#[test]
fn a_bad_request_names_its_field_and_a_lost_race_is_concurrent_modification() {
    let (status, context) = wire(DomainError::InvalidArgument {
        field: "reason",
        detail: "x".to_owned(),
        reason: reason::INVALID_REQUEST,
    });
    assert_eq!(status, 400);
    assert_eq!(context["field_violations"][0]["field"], "reason");
    assert_eq!(
        context["field_violations"][0]["reason"],
        reason::INVALID_REQUEST
    );

    let (status, context) = wire(DomainError::Concurrent("x".to_owned()));
    assert_eq!(status, 409);
    assert_eq!(context["reason"], reason::CONCURRENT_MODIFICATION);

    let (status, context) = wire(DomainError::IdempotencyKeyReused {
        key: "k".to_owned(),
        operation: Operation::ReratingsCreate,
    });
    assert_eq!(status, 409);
    assert!(context.get("reason").is_none(), "{context}");
}

#[test]
fn a_failure_that_is_not_the_callers_is_503_or_500() {
    #[derive(Debug, thiserror::Error)]
    #[error("commit failed")]
    struct CommitFailed;

    for unavailable in [
        DomainError::AuthzUnavailable("x".to_owned(), None),
        DomainError::Coordination("x".to_owned(), None),
    ] {
        assert_eq!(wire(unavailable).0, 503);
    }
    for internal in [
        DomainError::Database("x".to_owned(), None),
        DomainError::CorruptRow("x".to_owned()),
        DomainError::Transaction(Cause::new(CommitFailed)),
        DomainError::LeaderWorkEnded("x".to_owned(), None),
        DomainError::Internal("x".to_owned(), None),
    ] {
        assert_eq!(wire(internal).0, 500);
    }
}

#[test]
fn an_unavailable_dependency_tells_the_caller_when_to_retry() {
    for unavailable in [
        DomainError::AuthzUnavailable("PDP evaluation failed".to_owned(), None),
        DomainError::Coordination("no backend".to_owned(), None),
    ] {
        let (_, context) = wire(unavailable);
        assert_eq!(context["retry_after_seconds"], RETRY_AFTER_SECONDS);
    }
}

#[test]
fn internal_details_never_reach_the_caller() {
    let err = CanonicalError::from(DomainError::Database(
        "insert inbox entry: secret".to_owned(),
        None,
    ));
    assert!(!err.detail().contains("secret"));
}

#[test]
fn a_reused_idempotency_key_names_the_key_on_its_operations_resource() {
    let err = CanonicalError::from(DomainError::IdempotencyKeyReused {
        key: "order-17".to_owned(),
        operation: Operation::RetentionHoldsCreate,
    });
    assert_eq!(err.status_code(), 409);
    assert_eq!(err.resource_name(), Some("order-17"));
    assert_eq!(err.resource_type(), Some(labels::RETENTION_HOLD));
}

#[test]
fn an_infrastructure_error_keeps_its_cause_as_the_source() {
    #[derive(Debug, thiserror::Error)]
    #[error("connection reset")]
    struct Reset;
    let err = DomainError::Database("insert inbox entry".to_owned(), Some(Cause::new(Reset)));
    let source = std::error::Error::source(&err).expect("the cause is the source");
    assert_eq!(source.to_string(), "connection reset");
    // The cause stands in for the failure: the chain names it once and goes on with the failure's
    // own source, here none; the failure itself is still reachable for a caller to inspect.
    assert!(source.source().is_none(), "the failure is named once");
    let cause = source
        .downcast_ref::<Cause>()
        .expect("the source is the cause");
    assert!(cause.get().is::<Reset>());
}

#[test]
fn a_hidden_entry_names_no_tenant() {
    let err = CanonicalError::from(DomainError::NotFound {
        what: "inbox entry",
    });
    assert_eq!(err.detail(), "inbox entry not found");
}
