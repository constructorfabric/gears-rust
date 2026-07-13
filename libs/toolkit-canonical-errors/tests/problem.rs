extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::resource_error;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_gts::{gts_id, gts_uri};

const USER_RESOURCE: &str = gts_id!("cf.core.users.user.v1~");
const FUTURE_PROBLEM_TYPE: &str = gts_uri!("cf.future.errors.something_new.v1~");

#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct R;

#[resource_error(gts_id!("cf.core.test.resource.v1~"))]
struct TestR;

#[test]
fn problem_from_not_found_has_correct_fields() {
    let err = R::not_found("Resource not found")
        .with_resource("user-123")
        .create();
    let problem = Problem::from(err);
    assert_eq!(
        problem.problem_type,
        gts_uri!("cf.core.errors.err.v1~cf.core.err.not_found.v1~")
    );
    assert_eq!(problem.title, "Not Found");
    assert_eq!(problem.status, 404);
    assert_eq!(problem.detail, "Resource not found");
    assert_eq!(problem.context["resource_type"], USER_RESOURCE);
    assert_eq!(problem.context["resource_name"], "user-123");
}

#[test]
fn problem_json_excludes_none_fields() {
    let err = CanonicalError::service_unavailable()
        .with_retry_after_seconds(30)
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();
    assert!(json.get("trace_id").is_none());
}

#[test]
fn direct_constructor_has_no_resource_type() {
    let err = CanonicalError::service_unavailable()
        .with_retry_after_seconds(30)
        .create();
    assert_eq!(err.resource_type(), None);
    let _problem = Problem::from(err);
}

#[test]
fn problem_json_excludes_resource_type_when_none() {
    let err = CanonicalError::internal("some error").create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();
    assert!(json["context"].get("resource_type").is_none());
}

// =========================================================================
// diagnostic() accessor
// =========================================================================

#[test]
fn diagnostic_returns_description_for_internal() {
    let err = CanonicalError::internal("db pool exhausted").create();
    assert_eq!(err.diagnostic(), Some("db pool exhausted"));
}

#[test]
fn diagnostic_returns_description_for_unknown() {
    let err = TestR::unknown("unexpected upstream response").create();
    assert_eq!(err.diagnostic(), Some("unexpected upstream response"));
}

#[test]
fn diagnostic_returns_none_for_other_categories() {
    let err = TestR::not_found("gone").with_resource("x").create();
    assert_eq!(err.diagnostic(), None);
}

// =========================================================================
// from_error_debug() — non-production path
// =========================================================================

#[test]
fn from_error_debug_includes_description_for_internal() {
    let err = CanonicalError::internal("db pool exhausted").create();
    let problem = Problem::from_error_debug(&err).unwrap();
    assert_eq!(problem.context["description"], "db pool exhausted");
}

#[test]
fn from_error_debug_includes_description_for_unknown() {
    let err = TestR::unknown("unexpected upstream response").create();
    let problem = Problem::from_error_debug(&err).unwrap();
    assert_eq!(
        problem.context["description"],
        "unexpected upstream response"
    );
}

#[test]
fn from_error_does_not_include_description_for_internal() {
    let err = CanonicalError::internal("db pool exhausted").create();
    let problem = Problem::from_error(&err).unwrap();
    assert!(problem.context.get("description").is_none());
}

#[test]
fn from_error_does_not_include_description_for_unknown() {
    let err = TestR::unknown("unexpected upstream response").create();
    let problem = Problem::from_error(&err).unwrap();
    assert!(problem.context.get("description").is_none());
}

#[test]
fn from_error_debug_no_op_for_other_categories() {
    let err = TestR::not_found("gone").with_resource("x").create();
    let normal = Problem::from_error(&err).unwrap();
    let debug = Problem::from_error_debug(&err).unwrap();
    assert_eq!(
        serde_json::to_value(&normal).unwrap(),
        serde_json::to_value(&debug).unwrap(),
    );
}

// =========================================================================
// Round-trip: CanonicalError → Problem → JSON → Problem → CanonicalError
//
// Out-of-process SDK consumers receive `application/problem+json` over the
// wire, deserialize to `Problem`, and reconstruct `CanonicalError` via
// `TryFrom`. Tests below pin the round-trip for every canonical variant.
// =========================================================================

use toolkit_canonical_errors::ProblemConversionError;

/// Run a canonical error through the full out-of-process round-trip:
/// build → Problem → JSON bytes → deserialize → `TryFrom<Problem>`.
/// Returns the reconstructed `CanonicalError`.
#[allow(clippy::expect_used)] // Test helper: panics are the failure path.
fn round_trip(err: CanonicalError) -> CanonicalError {
    let problem = Problem::from(err);
    let bytes = serde_json::to_vec(&problem).expect("Problem serializes");
    let parsed: Problem = serde_json::from_slice(&bytes).expect("Problem deserializes");
    CanonicalError::try_from(parsed).expect("Problem reconstructs as CanonicalError")
}

#[test]
fn round_trip_invalid_argument_with_field_violation() {
    let original = R::invalid_argument()
        .with_field_violation("email", "must be a valid email", "INVALID_FORMAT")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::InvalidArgument {
            ctx,
            detail,
            resource_type,
            resource_name,
            ..
        } => {
            assert_eq!(detail, "Request validation failed");
            assert_eq!(resource_type.as_deref(), Some(USER_RESOURCE));
            assert!(resource_name.is_none());
            match ctx {
                toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations } => {
                    assert_eq!(field_violations.len(), 1);
                    assert_eq!(field_violations[0].field, "email");
                    assert_eq!(field_violations[0].reason, "INVALID_FORMAT");
                    assert_eq!(field_violations[0].description, "must be a valid email");
                }
                other => panic!("expected FieldViolations, got {other:?}"),
            }
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[test]
fn round_trip_invalid_argument_format_variant() {
    let original = R::invalid_argument().with_format("malformed JSON").create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::InvalidArgument {
            ctx: toolkit_canonical_errors::InvalidArgument::Format { format },
            ..
        } => assert_eq!(format, "malformed JSON"),
        other => panic!("expected InvalidArgument::Format, got {other:?}"),
    }
}

#[test]
fn round_trip_not_found_preserves_resource_type_and_name() {
    let original = R::not_found("User not found")
        .with_resource("user-42")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::NotFound {
            detail,
            resource_type,
            resource_name,
            ..
        } => {
            assert_eq!(detail, "User not found");
            assert_eq!(resource_type.as_deref(), Some(USER_RESOURCE));
            assert_eq!(resource_name.as_deref(), Some("user-42"));
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn round_trip_already_exists() {
    let original = R::already_exists("Duplicate user")
        .with_resource("alice@example.com")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::AlreadyExists {
            detail,
            resource_name,
            ..
        } => {
            assert_eq!(detail, "Duplicate user");
            assert_eq!(resource_name.as_deref(), Some("alice@example.com"));
        }
        other => panic!("expected AlreadyExists, got {other:?}"),
    }
}

#[test]
fn round_trip_unauthenticated_preserves_reason() {
    let original = CanonicalError::unauthenticated()
        .with_reason("EXPIRED_TOKEN")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::Unauthenticated { ctx, .. } => {
            assert_eq!(ctx.reason.as_deref(), Some("EXPIRED_TOKEN"));
        }
        other => panic!("expected Unauthenticated, got {other:?}"),
    }
}

#[test]
fn round_trip_unauthenticated_with_reason_only() {
    // `unauthenticated()` requires `.with_reason()` before `.create()`.
    // The "without reason" case (canonical context carries
    // `reason: None`) can't be built through the public builder API.
    let original = CanonicalError::unauthenticated()
        .with_reason("AUTH_REQUIRED")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::Unauthenticated { ctx, .. } => {
            assert_eq!(ctx.reason.as_deref(), Some("AUTH_REQUIRED"));
        }
        other => panic!("expected Unauthenticated, got {other:?}"),
    }
}

#[test]
fn round_trip_permission_denied_preserves_reason() {
    let original = R::permission_denied().with_reason("AUTHZ_DENIED").create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::PermissionDenied { ctx, .. } => {
            assert_eq!(ctx.reason, "AUTHZ_DENIED");
        }
        other => panic!("expected PermissionDenied, got {other:?}"),
    }
}

#[test]
fn round_trip_resource_exhausted_preserves_quota_violations() {
    let original = R::resource_exhausted("Rate limit exceeded")
        .with_quota_violation("requests_per_minute", "60/min exceeded")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::ResourceExhausted { ctx, .. } => {
            assert_eq!(ctx.violations.len(), 1);
            assert_eq!(ctx.violations[0].subject, "requests_per_minute");
            assert_eq!(ctx.violations[0].description, "60/min exceeded");
            assert!(ctx.violations[0].retry_after_seconds.is_none());
        }
        other => panic!("expected ResourceExhausted, got {other:?}"),
    }
}

#[test]
fn round_trip_resource_exhausted_preserves_quota_retry_after() {
    let original = R::resource_exhausted("Rate limit exceeded")
        .with_quota_violation("requests_per_minute", "60/min exceeded")
        .with_quota_violation_retry_after_seconds(15)
        .create();

    // First confirm the wire JSON shape — retry hint lives on the
    // individual violation, not at the category level.
    let problem = toolkit_canonical_errors::Problem::from(original.clone());
    let json = serde_json::to_value(&problem).expect("Problem serializes");
    assert_eq!(json["context"]["violations"][0]["retry_after_seconds"], 15);

    let restored = round_trip(original);
    match restored {
        CanonicalError::ResourceExhausted { ctx, .. } => {
            assert_eq!(ctx.violations.len(), 1);
            assert_eq!(ctx.violations[0].retry_after_seconds, Some(15));
        }
        other => panic!("expected ResourceExhausted, got {other:?}"),
    }
}

#[test]
fn round_trip_failed_precondition_preserves_violations() {
    let original = R::failed_precondition()
        .with_precondition_violation("subject_locked", "Account is locked", "STATE")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::FailedPrecondition { ctx, .. } => {
            assert_eq!(ctx.violations.len(), 1);
            assert_eq!(ctx.violations[0].type_, "STATE");
            assert_eq!(ctx.violations[0].subject, "subject_locked");
            assert_eq!(ctx.violations[0].description, "Account is locked");
        }
        other => panic!("expected FailedPrecondition, got {other:?}"),
    }
}

#[test]
fn round_trip_aborted_preserves_reason() {
    let original = R::aborted("concurrent modification")
        .with_reason("CONCURRENT_WRITE")
        .with_resource("user-42")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::Aborted { ctx, .. } => {
            assert_eq!(ctx.reason, "CONCURRENT_WRITE");
        }
        other => panic!("expected Aborted, got {other:?}"),
    }
}

#[test]
fn round_trip_out_of_range_preserves_field_violation() {
    let original = R::out_of_range("page must be 1..=100")
        .with_field_violation("page", "must be 1..=100", "OUT_OF_BOUNDS")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::OutOfRange { ctx, .. } => {
            assert_eq!(ctx.field_violations.len(), 1);
            assert_eq!(ctx.field_violations[0].field, "page");
            assert_eq!(ctx.field_violations[0].reason, "OUT_OF_BOUNDS");
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
}

#[test]
fn round_trip_service_unavailable_preserves_retry_after() {
    let original = CanonicalError::service_unavailable()
        .with_retry_after_seconds(30)
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::ServiceUnavailable { ctx, .. } => {
            assert_eq!(ctx.retry_after_seconds, Some(30));
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[test]
fn round_trip_service_unavailable_without_retry_after() {
    let original = CanonicalError::service_unavailable().create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::ServiceUnavailable { ctx, .. } => {
            assert!(ctx.retry_after_seconds.is_none());
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[test]
fn round_trip_internal_strips_diagnostic() {
    // `Internal.description` is `#[serde(skip)]` — production wire never
    // carries the diagnostic. Round-trip reconstructs as empty string.
    let original = CanonicalError::internal("server-side stack trace x").create();
    assert_eq!(original.diagnostic(), Some("server-side stack trace x"));

    let restored = round_trip(original);
    match restored {
        CanonicalError::Internal { ctx, detail, .. } => {
            assert!(
                ctx.description.is_empty(),
                "diagnostic should not survive the wire round-trip",
            );
            assert_eq!(detail, "An internal error occurred. Please retry later.");
        }
        other => panic!("expected Internal, got {other:?}"),
    }
}

#[test]
fn round_trip_unknown_strips_diagnostic() {
    let original = R::unknown("upstream returned malformed body").create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::Unknown { ctx, .. } => {
            assert!(ctx.description.is_empty());
        }
        other => panic!("expected Unknown, got {other:?}"),
    }
}

#[test]
fn round_trip_cancelled() {
    let original = R::cancelled().create();
    let restored = round_trip(original);
    assert!(matches!(restored, CanonicalError::Cancelled { .. }));
}

#[test]
fn round_trip_deadline_exceeded() {
    let original = R::deadline_exceeded("Operation timed out").create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::DeadlineExceeded { detail, .. } => {
            assert_eq!(detail, "Operation timed out");
        }
        other => panic!("expected DeadlineExceeded, got {other:?}"),
    }
}

#[test]
fn round_trip_unimplemented() {
    let original = R::unimplemented("not yet implemented").create();
    let restored = round_trip(original);
    assert!(matches!(restored, CanonicalError::Unimplemented { .. }));
}

#[test]
fn round_trip_data_loss() {
    let original = R::data_loss("Replica diverged")
        .with_resource("partition-7")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::DataLoss {
            detail,
            resource_name,
            ..
        } => {
            assert_eq!(detail, "Replica diverged");
            assert_eq!(resource_name.as_deref(), Some("partition-7"));
        }
        other => panic!("expected DataLoss, got {other:?}"),
    }
}

#[test]
fn try_from_unknown_problem_type_returns_error() {
    let problem = Problem {
        problem_type: FUTURE_PROBLEM_TYPE.to_owned(),
        title: "Future".to_owned(),
        status: 599,
        detail: "Not in canonical taxonomy".to_owned(),
        instance: None,
        trace_id: None,
        context: serde_json::json!({}),
    };
    let result = CanonicalError::try_from(problem);
    match result {
        Err(ProblemConversionError::UnknownProblemType(t)) => {
            assert_eq!(t, FUTURE_PROBLEM_TYPE);
        }
        other => panic!("expected UnknownProblemType, got {other:?}"),
    }
}

#[test]
fn try_from_unprefixed_problem_type_returns_error() {
    let problem = Problem {
        problem_type: "https://example.com/errors/foo".to_owned(),
        title: "Foo".to_owned(),
        status: 500,
        detail: "non-gts URI".to_owned(),
        instance: None,
        trace_id: None,
        context: serde_json::json!({}),
    };
    assert!(matches!(
        CanonicalError::try_from(problem),
        Err(ProblemConversionError::UnknownProblemType(_)),
    ));
}

#[test]
fn try_from_malformed_context_returns_error() {
    // Build a Problem with a valid problem_type but a context that can't
    // deserialize into the expected ResourceExhausted shape — `violations`
    // must be an array, not a number.
    let problem = Problem {
        problem_type: gts_uri!("cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~")
            .to_owned(),
        title: "Resource Exhausted".to_owned(),
        status: 429,
        detail: "Rate limit exceeded".to_owned(),
        instance: None,
        trace_id: None,
        context: serde_json::json!({ "violations": 42 }),
    };
    match CanonicalError::try_from(problem) {
        Err(ProblemConversionError::InvalidContext { category, .. }) => {
            assert_eq!(category, "resource_exhausted");
        }
        other => panic!("expected InvalidContext, got {other:?}"),
    }
}

#[test]
fn round_trip_preserves_resource_type_when_context_is_empty_struct() {
    // DeadlineExceeded has an empty `ctx: {}` and accepts an optional
    // resource scope — make sure resource_type/resource_name stamped into
    // the JSON survive the round-trip even though they're not part of the
    // context struct.
    let original = R::deadline_exceeded("Operation timed out")
        .with_resource("session-abc")
        .create();
    let restored = round_trip(original);
    match restored {
        CanonicalError::DeadlineExceeded {
            resource_type,
            resource_name,
            ..
        } => {
            assert_eq!(resource_type.as_deref(), Some(USER_RESOURCE));
            assert_eq!(resource_name.as_deref(), Some("session-abc"));
        }
        other => panic!("expected DeadlineExceeded, got {other:?}"),
    }
}
