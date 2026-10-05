use crate::domain::error::DomainError;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_db::DbError;
use toolkit_gts::gts_id;

const NOTE_RESOURCE_TYPE: &str = gts_id!("cf.construct.foundation.note.v1~");

fn wire(err: DomainError) -> Problem {
    Problem::from(CanonicalError::from(err))
}

#[test]
fn not_found_maps_to_404_with_resource_type() {
    let problem = wire(DomainError::NotFound);

    assert_eq!(problem.status, Some(404));
    assert_eq!(
        problem
            .context
            .get("resource_type")
            .and_then(|v| v.as_str()),
        Some(NOTE_RESOURCE_TYPE),
    );
}

#[test]
fn validation_maps_to_400_with_field_violation() {
    let problem = wire(DomainError::validation("text", "must not be empty"));

    assert_eq!(problem.status, Some(400));
    let violation = problem
        .context
        .get("field_violations")
        .and_then(|v| v.get(0))
        .expect("expected a field violation");
    assert_eq!(
        violation.get("field").and_then(|v| v.as_str()),
        Some("text")
    );
}

#[test]
fn forbidden_maps_to_403_without_the_internal_message() {
    let problem = wire(DomainError::forbidden("denied by policy"));

    assert_eq!(problem.status, Some(403));
    assert_eq!(
        problem.context.get("reason").and_then(|v| v.as_str()),
        Some("ACCESS_DENIED"),
    );
    // The internal message stays out of the whole response, not only `detail`.
    assert!(!format!("{problem:?}").contains("denied by policy"));
}

#[test]
fn internal_maps_to_500_without_leaking_detail() {
    let problem = wire(DomainError::internal("connection string is secret"));

    assert_eq!(problem.status, Some(500));
    assert!(!problem.detail.contains("secret"));
}

#[test]
fn database_maps_to_500_without_leaking_detail() {
    let problem = wire(DomainError::Database(DbError::InvalidConfig(
        "password=secret".to_owned(),
    )));

    assert_eq!(problem.status, Some(500));
    assert!(!problem.detail.contains("secret"));
}
