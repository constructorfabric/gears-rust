extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::resource_error;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_gts::{GtsId, gts_id};

#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct R;

#[test]
fn not_found_gts_type() {
    let err = R::not_found("Resource not found")
        .with_resource("user-123")
        .create();
    assert_eq!(
        err.gts_type(),
        gts_id!("cf.core.errors.err.v1~cf.core.err.not_found.v1~")
    );
}

#[test]
fn not_found_status_code() {
    let err = R::not_found("Resource not found")
        .with_resource("user-123")
        .create();
    assert_eq!(err.status_code(), 404);
}

#[test]
fn not_found_title() {
    let err = R::not_found("Resource not found")
        .with_resource("user-123")
        .create();
    assert_eq!(err.title(), "Not Found");
}

#[test]
fn display_includes_category_and_detail() {
    let err = R::not_found("User not found")
        .with_resource("user-123")
        .create();
    assert_eq!(format!("{err}"), "not_found: User not found");
}

#[test]
fn with_detail_overrides_default() {
    let err = R::not_found("custom detail")
        .with_resource("user-123")
        .create();
    assert_eq!(err.detail(), "custom detail");
}

#[test]
fn all_16_categories_convert_to_problem() {
    let errors: Vec<CanonicalError> = vec![
        R::cancelled().create(),
        R::unknown("unknown error").create(),
        R::invalid_argument()
            .with_field_violation("field", "bad format", "INVALID_FORMAT")
            .create(),
        R::deadline_exceeded("timed out").create(),
        R::not_found("Resource not found")
            .with_resource("user-123")
            .create(),
        R::already_exists("Resource already exists")
            .with_resource("user-123")
            .create(),
        R::permission_denied()
            .with_reason("INSUFFICIENT_ROLE")
            .create(),
        R::resource_exhausted("Quota exceeded")
            .with_quota_violation("requests", "limit reached")
            .create(),
        R::failed_precondition()
            .with_precondition_violation("state", "not ready", "STATE")
            .create(),
        R::aborted("concurrency conflict")
            .with_reason("OPTIMISTIC_LOCK_FAILURE")
            .create(),
        R::out_of_range("Value out of range")
            .with_field_violation("page", "beyond last page", "OUT_OF_RANGE")
            .create(),
        R::unimplemented("not implemented").create(),
        CanonicalError::internal("bug").create(),
        CanonicalError::service_unavailable()
            .with_retry_after_seconds(10)
            .create(),
        R::data_loss("data loss").with_resource("user-123").create(),
        CanonicalError::unauthenticated()
            .with_reason("MISSING_CREDENTIALS")
            .create(),
    ];
    assert_eq!(errors.len(), 16);
    for err in errors {
        let problem = Problem::from(err);
        assert!(!problem.problem_type.is_empty());
        assert!(!problem.title.is_empty());
        assert!(problem.status > 0);
    }
}

// =========================================================================
// From impls for common library errors
// =========================================================================

#[test]
fn from_io_error_produces_internal() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
    let err = CanonicalError::from(io_err);
    assert_eq!(err.status_code(), 500);
    assert_eq!(err.title(), "Internal");
    assert_eq!(
        err.gts_type(),
        gts_id!("cf.core.errors.err.v1~cf.core.err.internal.v1~")
    );
}

#[test]
fn from_serde_json_error_produces_invalid_argument() {
    // DESIGN.md §1.1 maps `serde_json::Error` to `InvalidArgument` (400):
    // a malformed JSON body is a client fault, not a server one.
    let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
    let raw_msg = json_err.to_string();
    let err = CanonicalError::from(json_err);
    assert_eq!(err.status_code(), 400);
    assert_eq!(err.title(), "Invalid Argument");
    assert_eq!(err.detail(), raw_msg);
    assert_eq!(
        err.gts_type(),
        gts_id!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
    );
    assert_eq!(err.diagnostic(), None);
}

#[test]
fn question_mark_propagation_io() {
    fn inner() -> Result<(), CanonicalError> {
        let _: Vec<u8> = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "access denied",
        ))?;
        Ok(())
    }
    let err = inner().unwrap_err();
    assert_eq!(err.status_code(), 500);
}

#[test]
fn question_mark_propagation_serde_json() {
    fn inner() -> Result<serde_json::Value, CanonicalError> {
        Ok(serde_json::from_str("{invalid")?)
    }
    let err = inner().unwrap_err();
    assert_eq!(err.status_code(), 400);
    assert_eq!(
        err.gts_type(),
        gts_id!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
    );
}

// =========================================================================
// GTS ID validation — ensures all IDs in the crate are valid GTS identifiers
// =========================================================================

#[test]
fn validate_all_gts_ids() {
    let errors = vec![
        R::cancelled().create(),
        R::unknown("e").create(),
        R::invalid_argument()
            .with_field_violation("f", "bad", "INVALID")
            .create(),
        R::deadline_exceeded("timed out").create(),
        R::not_found("not found").with_resource("user-123").create(),
        R::already_exists("exists")
            .with_resource("user-123")
            .create(),
        R::permission_denied()
            .with_reason("INSUFFICIENT_ROLE")
            .create(),
        R::resource_exhausted("quota")
            .with_quota_violation("req", "limit")
            .create(),
        R::failed_precondition()
            .with_precondition_violation("s", "d", "STATE")
            .create(),
        R::aborted("conflict")
            .with_reason("OPTIMISTIC_LOCK_FAILURE")
            .create(),
        R::out_of_range("range")
            .with_field_violation("page", "too high", "OUT_OF_RANGE")
            .create(),
        R::unimplemented("n").create(),
        CanonicalError::internal("d").create(),
        CanonicalError::service_unavailable()
            .with_retry_after_seconds(1)
            .create(),
        R::data_loss("d").with_resource("user-123").create(),
        CanonicalError::unauthenticated()
            .with_reason("MISSING_CREDENTIALS")
            .create(),
    ];
    for err in &errors {
        let id = err.gts_type();
        assert!(id.ends_with('~'), "GTS type ID must end with ~: {id}");
        GtsId::try_new(id).unwrap_or_else(|e| panic!("Invalid GTS type ID '{id}': {e}"));
    }
}
