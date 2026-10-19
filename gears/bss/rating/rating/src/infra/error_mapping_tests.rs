use uuid::Uuid;

use super::*;
use crate::domain::error::Cause;
use crate::domain::inbox::InboxSource;

fn status(err: DomainError) -> u16 {
    CanonicalError::from(err).status_code()
}

#[test]
fn every_domain_error_has_its_status() {
    assert_eq!(
        status(DomainError::NotFound {
            what: "inbox entry"
        }),
        404
    );
    assert_eq!(
        status(DomainError::NotQuarantined {
            entry_id: Uuid::nil()
        }),
        400
    );
    assert_eq!(
        status(DomainError::SourceNotApplicable {
            inbox_source: InboxSource::Fact
        }),
        400
    );
    assert_eq!(
        status(DomainError::ProducerNotVerified {
            inbox_source: InboxSource::Fact
        }),
        403
    );
    assert_eq!(
        status(DomainError::OutsideGrant {
            tenant_id: Uuid::nil()
        }),
        403
    );
    assert_eq!(
        status(DomainError::IdempotencyKeyReused {
            key: "k".to_owned()
        }),
        409
    );
    assert_eq!(
        status(DomainError::InvalidArgument {
            field: "reason",
            detail: "x".to_owned(),
            reason: "INVALID_REQUEST"
        }),
        400
    );
    assert_eq!(status(DomainError::Concurrent("x".to_owned())), 409);
    assert_eq!(status(DomainError::PermissionDenied("x".to_owned())), 403);
    assert_eq!(
        status(DomainError::AuthzUnavailable("x".to_owned(), None)),
        503
    );
    assert_eq!(status(DomainError::Coordination("x".to_owned(), None)), 503);
    assert_eq!(status(DomainError::Database("x".to_owned(), None)), 500);
    assert_eq!(status(DomainError::LeaderWorkEnded("x".to_owned())), 500);
    assert_eq!(status(DomainError::CorruptRow("x".to_owned())), 500);
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
fn a_reused_idempotency_key_names_the_key_as_the_resource() {
    let err = CanonicalError::from(DomainError::IdempotencyKeyReused {
        key: "order-17".to_owned(),
    });
    assert_eq!(err.status_code(), 409);
    let rendered = format!("{err:?}");
    assert!(rendered.contains("order-17"), "{rendered}");
}

#[test]
fn an_infrastructure_error_keeps_its_cause_as_the_source() {
    #[derive(Debug, thiserror::Error)]
    #[error("connection reset")]
    struct Reset;
    let err = DomainError::Database("insert inbox entry".to_owned(), Some(Cause::new(Reset)));
    let source = std::error::Error::source(&err).expect("the cause is the source");
    assert_eq!(source.to_string(), "connection reset");
}

#[test]
fn a_hidden_entry_names_no_tenant() {
    let err = CanonicalError::from(DomainError::NotFound {
        what: "inbox entry",
    });
    assert_eq!(err.detail(), "inbox entry not found");
}
