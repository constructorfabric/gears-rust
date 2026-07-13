extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::resource_error;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_gts::{gts_id, gts_uri};

const USER_RESOURCE: &str = gts_id!("cf.core.users.user.v1~");
const FILE_RESOURCE: &str = gts_id!("cf.core.files.file.v1~");
const TENANT_RESOURCE: &str = gts_id!("cf.core.tenants.tenant.v1~");
const UPSTREAM_RESOURCE: &str = gts_id!("cf.oagw.upstreams.upstream.v1~");

#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct UserResourceError;

#[resource_error(gts_id!("cf.core.files.file.v1~"))]
struct FileResourceError;

#[resource_error(gts_id!("cf.core.tenants.tenant.v1~"))]
struct TenantResourceError;

#[resource_error(gts_id!("cf.oagw.upstreams.upstream.v1~"))]
struct UpstreamResourceError;

// =========================================================================
// Showcase tests — resource-scoped categories (macro-generated)
// =========================================================================

#[test]
fn showcase_not_found() {
    let err = UserResourceError::not_found("User not found")
        .with_resource("user-123")
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.not_found.v1~"),
            "title": "Not Found",
            "status": 404,
            "detail": "User not found",
            "context": {
                "resource_type": USER_RESOURCE,
                "resource_name": "user-123"
            }
        })
    );
}

#[test]
fn showcase_already_exists() {
    let err = UserResourceError::already_exists("User already exists")
        .with_resource("alice@example.com")
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.already_exists.v1~"),
            "title": "Already Exists",
            "status": 409,
            "detail": "User already exists",
            "context": {
                "resource_type": USER_RESOURCE,
                "resource_name": "alice@example.com"
            }
        })
    );
}

#[test]
fn showcase_data_loss() {
    let err = FileResourceError::data_loss("Data loss detected")
        .with_resource("01JFILE-ABC")
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.data_loss.v1~"),
            "title": "Data Loss",
            "status": 500,
            "detail": "Data loss detected",
            "context": {
                "resource_type": FILE_RESOURCE,
                "resource_name": "01JFILE-ABC"
            }
        })
    );
}

#[test]
fn showcase_invalid_argument() {
    let err = UserResourceError::invalid_argument()
        .with_field_violation("email", "must be a valid email address", "INVALID_FORMAT")
        .with_field_violation("age", "must be at least 18", "OUT_OF_RANGE")
        .create();

    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~"),
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "context": {
                "resource_type": USER_RESOURCE,
                "field_violations": [
                    {
                        "field": "email",
                        "description": "must be a valid email address",
                        "reason": "INVALID_FORMAT"
                    },
                    {
                        "field": "age",
                        "description": "must be at least 18",
                        "reason": "OUT_OF_RANGE"
                    }
                ]
            }
        })
    );
}

#[test]
fn showcase_invalid_argument_format() {
    let err = UserResourceError::invalid_argument()
        .with_format("Request body is not valid JSON")
        .create();

    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~"),
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request body is not valid JSON",
            "context": {
                "resource_type": USER_RESOURCE,
                "format": "Request body is not valid JSON"
            }
        })
    );
}

#[test]
fn showcase_invalid_argument_constraint() {
    let err = UserResourceError::invalid_argument()
        .with_constraint("at most 10 tags allowed per resource")
        .create();

    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~"),
            "title": "Invalid Argument",
            "status": 400,
            "detail": "at most 10 tags allowed per resource",
            "context": {
                "resource_type": USER_RESOURCE,
                "constraint": "at most 10 tags allowed per resource"
            }
        })
    );
}

#[test]
fn showcase_invalid_argument_format_with_resource() {
    let err = UserResourceError::invalid_argument()
        .with_resource("user-123")
        .with_format("Request body is not valid JSON")
        .create();

    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~"),
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request body is not valid JSON",
            "context": {
                "resource_type": USER_RESOURCE,
                "resource_name": "user-123",
                "format": "Request body is not valid JSON"
            }
        })
    );
}

#[test]
fn showcase_out_of_range() {
    let err = UserResourceError::out_of_range("Page out of range")
        .with_field_violation(
            "page",
            "Page 50 is beyond the last page (12)",
            "OUT_OF_RANGE",
        )
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.out_of_range.v1~"),
            "title": "Out of Range",
            "status": 400,
            "detail": "Page out of range",
            "context": {
                "resource_type": USER_RESOURCE,
                "field_violations": [
                    {
                        "field": "page",
                        "description": "Page 50 is beyond the last page (12)",
                        "reason": "OUT_OF_RANGE"
                    }
                ]
            }
        })
    );
}

#[test]
fn showcase_permission_denied() {
    let err = TenantResourceError::permission_denied()
        .with_reason("CROSS_TENANT_ACCESS")
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.permission_denied.v1~"),
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "context": {
                "resource_type": TENANT_RESOURCE,
                "reason": "CROSS_TENANT_ACCESS"
            }
        })
    );
}

#[test]
fn showcase_aborted() {
    let err = UpstreamResourceError::aborted("Operation aborted due to concurrency conflict")
        .with_reason("OPTIMISTIC_LOCK_FAILURE")
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.aborted.v1~"),
            "title": "Aborted",
            "status": 409,
            "detail": "Operation aborted due to concurrency conflict",
            "context": {
                "resource_type": UPSTREAM_RESOURCE,
                "reason": "OPTIMISTIC_LOCK_FAILURE"
            }
        })
    );
}

#[test]
fn showcase_unimplemented() {
    let err = UserResourceError::unimplemented("This operation is not implemented").create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.unimplemented.v1~"),
            "title": "Unimplemented",
            "status": 501,
            "detail": "This operation is not implemented",
            "context": {
                "resource_type": USER_RESOURCE
            }
        })
    );
}

#[test]
fn showcase_failed_precondition() {
    let err = TenantResourceError::failed_precondition()
        .with_precondition_violation(
            "tenant.users",
            "Tenant must have zero active users before deletion",
            "STATE",
        )
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~"),
            "title": "Failed Precondition",
            "status": 400,
            "detail": "Operation precondition not met",
            "context": {
                "resource_type": TENANT_RESOURCE,
                "violations": [
                    {
                        "type": "STATE",
                        "subject": "tenant.users",
                        "description": "Tenant must have zero active users before deletion"
                    }
                ]
            }
        })
    );
}

#[test]
fn showcase_internal() {
    let err = CanonicalError::internal("An internal error occurred. Please retry later.").create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.internal.v1~"),
            "title": "Internal",
            "status": 500,
            "detail": "An internal error occurred. Please retry later.",
            "context": {
            }
        })
    );
}

#[test]
fn showcase_deadline_exceeded() {
    let err = UserResourceError::deadline_exceeded("Request timed out").create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~"),
            "title": "Deadline Exceeded",
            "status": 504,
            "detail": "Request timed out",
            "context": {
                "resource_type": USER_RESOURCE
            }
        })
    );
}

#[test]
fn showcase_cancelled() {
    let err = UserResourceError::cancelled().create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.cancelled.v1~"),
            "title": "Cancelled",
            "status": 499,
            "detail": "Operation cancelled by the client",
            "context": {
                "resource_type": USER_RESOURCE
            }
        })
    );
}

// =========================================================================
// Showcase tests — system-level categories (direct constructors)
// =========================================================================

#[test]
fn showcase_unauthenticated() {
    let err = CanonicalError::unauthenticated()
        .with_reason("TOKEN_EXPIRED")
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~"),
            "title": "Unauthenticated",
            "status": 401,
            "detail": "Authentication required",
            "context": {
                "reason": "TOKEN_EXPIRED"
            }
        })
    );
}

#[test]
fn showcase_resource_exhausted() {
    let err = UserResourceError::resource_exhausted("Quota exceeded")
        .with_quota_violation(
            "requests_per_minute",
            "Limit of 100 requests per minute exceeded",
        )
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"),
            "title": "Resource Exhausted",
            "status": 429,
            "detail": "Quota exceeded",
            "context": {
                "resource_type": USER_RESOURCE,
                "violations": [
                    {
                        "subject": "requests_per_minute",
                        "description": "Limit of 100 requests per minute exceeded"
                    }
                ]
            }
        })
    );
}

#[test]
fn showcase_unavailable() {
    let err = CanonicalError::service_unavailable()
        .with_retry_after_seconds(30)
        .create();

    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~"),
            "title": "Service Unavailable",
            "status": 503,
            "detail": "Service temporarily unavailable",
            "context": {
                "retry_after_seconds": 30
            }
        })
    );
}

#[test]
fn showcase_unknown() {
    let err = UserResourceError::unknown("Unexpected response from payment provider").create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "type": gts_uri!("cf.core.errors.err.v1~cf.core.err.unknown.v1~"),
            "title": "Unknown",
            "status": 500,
            "detail": "An unknown error occurred",
            "context": {
                "resource_type": USER_RESOURCE
            }
        })
    );
}
