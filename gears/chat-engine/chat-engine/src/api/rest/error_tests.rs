use super::*;
use chat_engine_sdk::error::PluginError;
use std::time::Duration;
use toolkit_canonical_errors::Problem;

const NOT_FOUND_TYPE: &str = "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~";
const INVALID_ARGUMENT_TYPE: &str =
    "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~";
const PERMISSION_DENIED_TYPE: &str =
    "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~";
const ALREADY_EXISTS_TYPE: &str = "gts://gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~";
const SERVICE_UNAVAILABLE_TYPE: &str =
    "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~";
const INTERNAL_TYPE: &str = "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~";
const UNIMPLEMENTED_TYPE: &str = "gts://gts.cf.core.errors.err.v1~cf.core.err.unimplemented.v1~";

fn problem_from(err: ChatEngineError) -> Problem {
    Problem::from(CanonicalError::from(err))
}

#[test]
fn not_found_maps_to_404() {
    let p = problem_from(ChatEngineError::not_found("session", "abc"));
    assert_eq!(p.status, 404);
    assert_eq!(p.problem_type, NOT_FOUND_TYPE);
}

#[test]
fn forbidden_maps_to_403_with_permission_denied() {
    let p = problem_from(ChatEngineError::forbidden("missing scope"));
    assert_eq!(p.status, 403);
    assert_eq!(p.problem_type, PERMISSION_DENIED_TYPE);
}

#[test]
fn conflict_maps_to_409_already_exists() {
    let p = problem_from(ChatEngineError::conflict("invalid lifecycle transition"));
    assert_eq!(p.status, 409);
    assert_eq!(p.problem_type, ALREADY_EXISTS_TYPE);
}

#[test]
fn bad_request_maps_to_400_invalid_argument() {
    let p = problem_from(ChatEngineError::bad_request("missing 'content'"));
    assert_eq!(p.status, 400);
    assert_eq!(p.problem_type, INVALID_ARGUMENT_TYPE);
}

#[test]
fn backend_unavailable_without_plugin_err_maps_to_503() {
    let err = ChatEngineError::BackendUnavailable {
        reason: "upstream 502".into(),
        retry_after: None,
        source: None,
    };
    let p = problem_from(err);
    assert_eq!(p.status, 503);
    assert_eq!(p.problem_type, SERVICE_UNAVAILABLE_TYPE);
}

#[test]
fn backend_unavailable_rate_limited_with_retry_after_emits_503_with_hint() {
    let err: ChatEngineError = PluginError::rate_limited(Some(Duration::from_secs(7))).into();
    let p = problem_from(err);
    assert_eq!(p.status, 503);
    assert_eq!(p.problem_type, SERVICE_UNAVAILABLE_TYPE);
    assert_eq!(p.context["retry_after_seconds"].as_u64(), Some(7));
}

#[test]
fn backend_unavailable_redacts_non_user_facing_detail() {
    // `Transient` carries an operator-only message; the wire detail
    // must be generic.
    let err: ChatEngineError = PluginError::transient("internal hostname leaked").into();
    let p = problem_from(err);
    assert_eq!(p.status, 503);
    assert_eq!(p.problem_type, SERVICE_UNAVAILABLE_TYPE);
    // We can't assert detail == "Backend unavailable" verbatim — the
    // ServiceUnavailable builder constructs its own detail — but we
    // CAN assert the operator-only string never reached the wire.
    let body = serde_json::to_string(&p).unwrap();
    assert!(
        !body.contains("internal hostname leaked"),
        "operator-only detail must never appear on the wire: {body}"
    );
}

#[test]
fn not_implemented_maps_to_501() {
    let p = problem_from(ChatEngineError::not_implemented(
        "export storage backend not configured",
    ));
    assert_eq!(p.status, 501);
    assert_eq!(p.problem_type, UNIMPLEMENTED_TYPE);
}

#[test]
fn internal_maps_to_500_and_redacts_reason() {
    let err = ChatEngineError::Internal {
        reason: "DB connection pool exhausted".into(),
        source: None,
    };
    let p = problem_from(err);
    assert_eq!(p.status, 500);
    assert_eq!(p.problem_type, INTERNAL_TYPE);
    let body = serde_json::to_string(&p).unwrap();
    assert!(
        !body.contains("DB connection pool exhausted"),
        "internal `reason` must never appear on the wire: {body}"
    );
}
