use crate::api::rest::error::for_rest;
use crate::domain::error::DomainError;
use crate::domain::record_intake::{Place, Refusal, RefusalReason};
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_db::DbError;
use toolkit_gts::gts_id;

const RECORD_RESOURCE_TYPE: &str = gts_id!("cf.connectors.core.record.v1~");
const CHAT: &str = "gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~";

fn wire(err: DomainError) -> Problem {
    Problem::from(CanonicalError::from(err))
}

/// Everything a caller can read in a `Problem`, as one string.
fn visible_text(problem: &Problem) -> String {
    format!(
        "{} {} {} {}",
        problem.problem_type, problem.title, problem.detail, problem.context
    )
}

fn first_violation(problem: &Problem) -> &serde_json::Value {
    problem
        .context
        .get("field_violations")
        .and_then(|v| v.get(0))
        .expect("expected a field violation")
}

fn rest(err: DomainError) -> Problem {
    Problem::from(for_rest(err))
}

fn schema_violation() -> DomainError {
    DomainError::Refused(Refusal::new(
        RefusalReason::SchemaViolation,
        Some(CHAT),
        Place::pointer("/payload/role"),
        "enum",
    ))
}

#[test]
fn refused_on_the_route_is_422_naming_the_type_the_place_and_the_rule() {
    let problem = rest(schema_violation());

    assert_eq!(problem.status, Some(422));
    let violation = first_violation(&problem);
    assert_eq!(violation["field"], "/payload/role");
    assert_eq!(violation["description"], "enum");
    assert_eq!(violation["reason"], construct_sdk::reason::SCHEMA_VIOLATION);
    assert_eq!(
        problem
            .context
            .get("resource_type")
            .and_then(|v| v.as_str()),
        Some(RECORD_RESOURCE_TYPE),
    );
    assert_eq!(
        problem
            .context
            .get("resource_name")
            .and_then(|v| v.as_str()),
        Some(CHAT),
    );
}

#[test]
fn refused_for_an_in_process_caller_carries_no_http_status() {
    let error = CanonicalError::from(schema_violation());

    assert!(
        matches!(error, CanonicalError::InvalidArgument { .. }),
        "{error:?}"
    );
    // Without the REST override, the category's own status applies.
    assert_eq!(Problem::from(error).status, Some(400));
}

#[test]
fn a_refusal_of_the_whole_record_names_the_record_as_the_place() {
    let problem = rest(DomainError::Refused(Refusal::new(
        RefusalReason::ConnectorOff,
        None,
        Place::Whole,
        "the connector is off for the tenant",
    )));

    assert_eq!(problem.status, Some(422));
    let violation = first_violation(&problem);
    assert_eq!(violation["field"], "(record)");
    assert_eq!(violation["reason"], construct_sdk::reason::CONNECTOR_OFF);
    assert_eq!(
        problem
            .context
            .get("resource_name")
            .and_then(|v| v.as_str()),
        Some("record"),
    );
}

#[test]
fn other_errors_map_the_same_on_the_route_and_in_process() {
    for (on_route, in_process) in [
        (
            rest(DomainError::forbidden("x")),
            wire(DomainError::forbidden("x")),
        ),
        (
            rest(DomainError::Unavailable("x".to_owned())),
            wire(DomainError::Unavailable("x".to_owned())),
        ),
    ] {
        assert_eq!(on_route.status, in_process.status);
    }
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
    assert!(!visible_text(&problem).contains("denied by policy"));
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

#[test]
fn unavailable_maps_to_503_without_leaking_detail() {
    let problem = wire(DomainError::Unavailable(
        "pdp at 10.0.0.7 timed out".to_owned(),
    ));

    assert_eq!(problem.status, Some(503));
    assert!(!visible_text(&problem).contains("10.0.0.7"));
}
