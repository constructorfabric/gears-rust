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

#[test]
fn settings_errors_about_the_request_name_the_settings_resource() {
    use crate::api::rest::error::for_settings;

    // A refusal of another subject is the one error about the request the
    // settings routes give; a bad body is refused by the extractor.
    let cases = [(DomainError::forbidden("subject a may not read b"), 403)];
    for (error, status) in cases {
        let label = format!("{error:?}");
        let problem = Problem::from(for_settings(error));
        assert_eq!(problem.status, Some(status), "{label}");
        assert_eq!(
            problem
                .context
                .get("resource_type")
                .and_then(|v| v.as_str()),
            Some(crate::domain::subject_settings::SUBJECT_SETTINGS_TYPE),
            "{label}"
        );
        assert!(!visible_text(&problem).contains("may not read"), "{label}");
    }
}

#[test]
fn settings_failures_of_the_gear_map_as_everywhere_else() {
    use crate::api::rest::error::for_settings;

    let unavailable = Problem::from(for_settings(DomainError::Unavailable("down".to_owned())));
    let internal = Problem::from(for_settings(DomainError::internal("boom")));

    assert_eq!(unavailable.status, Some(503));
    assert_eq!(internal.status, Some(500));
}

/// A scope refusal with distinct ids, and the ids, none of which may reach the
/// answer.
fn out_of_scope() -> (DomainError, [uuid::Uuid; 4]) {
    use crate::domain::settings_scope::{ScopeRefusal, SettingsOutOfScope};

    let ids = [
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
    ];
    let error = DomainError::OutOfScope(SettingsOutOfScope {
        caller: ids[0],
        caller_tenant: ids[1],
        tenant_id: ids[2],
        subject_id: ids[3],
        refusal: ScopeRefusal::NotCovered,
    });
    (error, ids)
}

fn assert_access_denied_without(problem: &Problem, resource_type: &str, ids: &[uuid::Uuid]) {
    assert_eq!(problem.status, Some(403));
    assert_eq!(
        problem.context.get("reason").and_then(|v| v.as_str()),
        Some("ACCESS_DENIED"),
    );
    assert_eq!(
        problem
            .context
            .get("resource_type")
            .and_then(|v| v.as_str()),
        Some(resource_type),
    );
    let text = visible_text(problem);
    for id in ids {
        assert!(!text.contains(&id.to_string()), "{id} in {text}");
    }
}

#[test]
fn an_out_of_scope_settings_route_error_is_403_on_the_settings_resource_without_its_ids() {
    use crate::api::rest::error::for_settings;

    let (error, ids) = out_of_scope();
    let problem = Problem::from(for_settings(error));

    assert_access_denied_without(
        &problem,
        crate::domain::subject_settings::SUBJECT_SETTINGS_TYPE,
        &ids,
    );
}

#[test]
fn an_out_of_scope_error_on_the_record_path_is_403_without_its_ids() {
    let (on_route, ids) = out_of_scope();
    let (in_process, other_ids) = out_of_scope();

    assert_access_denied_without(&rest(on_route), RECORD_RESOURCE_TYPE, &ids);
    assert_access_denied_without(&wire(in_process), RECORD_RESOURCE_TYPE, &other_ids);
}

#[test]
fn an_out_of_scope_error_on_the_facts_route_answers_as_no_such_subject() {
    use crate::api::rest::error::for_facts;

    let (error, ids) = out_of_scope();
    let subject_id = ids[3];
    let problem = Problem::from(for_facts(error, subject_id));

    assert_eq!(problem.status, Some(404));
    let text = visible_text(&problem);
    for id in &ids[..3] {
        assert!(!text.contains(&id.to_string()), "{id} in {text}");
    }
}
