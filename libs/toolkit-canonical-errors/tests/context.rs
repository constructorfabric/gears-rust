extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::{
    Aborted, AlreadyExists, Cancelled, DataLoss, DeadlineExceeded, FailedPrecondition,
    FieldViolation, Internal, InvalidArgument, NotFound, OutOfRange, PermissionDenied,
    PreconditionViolation, QuotaViolation, ResourceExhausted, ServiceUnavailable, Unauthenticated,
    Unimplemented, Unknown,
};

// =========================================================================
// Shared inner types
// =========================================================================

#[test]
fn field_violation_serialization() {
    let v = FieldViolation::new("email", "must be valid", "INVALID_FORMAT");
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(json["field"], "email");
    assert_eq!(json["description"], "must be valid");
    assert_eq!(json["reason"], "INVALID_FORMAT");
}

#[test]
fn quota_violation_serialization() {
    let v = QuotaViolation::new("requests_per_minute", "Limit exceeded");
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(json["subject"], "requests_per_minute");
    assert_eq!(json["description"], "Limit exceeded");
}

#[test]
fn precondition_violation_serialization() {
    let v = PreconditionViolation::new("STATE", "tenant.users", "Must have zero users");
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(json["type"], "STATE");
    assert_eq!(json["subject"], "tenant.users");
    assert_eq!(json["description"], "Must have zero users");
}

// =========================================================================
// Per-category context serialization tests
// =========================================================================

#[test]
fn cancelled_serialization() {
    let ctx = Cancelled::new();
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
}

#[test]
fn unknown_serialization() {
    let ctx = Unknown::new("something went wrong");
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
}

#[test]
fn invalid_argument_field_violations_serialization() {
    let ctx = InvalidArgument::fields(vec![FieldViolation::new(
        "email",
        "must be valid",
        "INVALID_FORMAT",
    )]);
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json["field_violations"].is_array());
    assert_eq!(json["field_violations"][0]["field"], "email");
}

#[test]
fn invalid_argument_format_serialization() {
    let ctx = InvalidArgument::format("bad json");
    let json = serde_json::to_value(&ctx).unwrap();
    assert_eq!(json["format"], "bad json");
}

#[test]
fn invalid_argument_constraint_serialization() {
    let ctx = InvalidArgument::constraint("too many items");
    let json = serde_json::to_value(&ctx).unwrap();
    assert_eq!(json["constraint"], "too many items");
}

#[test]
fn deadline_exceeded_serialization() {
    let ctx = DeadlineExceeded::new();
    let json = serde_json::to_value(&ctx).unwrap();
    assert_eq!(json, serde_json::json!({}));
}

#[test]
fn deadline_exceeded_with_reason_serialization() {
    let ctx = DeadlineExceeded::new().with_reason("REQUEST_MAY_HAVE_BEEN_SENT");
    let json = serde_json::to_value(&ctx).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "reason": "REQUEST_MAY_HAVE_BEEN_SENT" })
    );
}

#[test]
fn deadline_exceeded_deserializes_without_reason() {
    let ctx: DeadlineExceeded = serde_json::from_str("{}").unwrap();
    assert!(ctx.reason.is_none());
}

#[test]
fn an_empty_reason_is_none_on_the_wire() {
    // Read: an empty wire value is no reason.
    let ctx: DeadlineExceeded = serde_json::from_str(r#"{"reason":""}"#).unwrap();
    assert!(ctx.reason.is_none());
    let ctx: ServiceUnavailable =
        serde_json::from_str(r#"{"retry_after_seconds":5,"reason":""}"#).unwrap();
    assert!(ctx.reason.is_none());

    // Write: an empty value set on the field directly is not sent.
    let mut ctx = DeadlineExceeded::new();
    ctx.reason = Some(String::new());
    assert_eq!(serde_json::to_value(&ctx).unwrap(), serde_json::json!({}));
    let mut ctx = ServiceUnavailable::new(Some(5));
    ctx.reason = Some(String::new());
    assert_eq!(
        serde_json::to_value(&ctx).unwrap(),
        serde_json::json!({ "retry_after_seconds": 5 })
    );
}

#[test]
fn not_found_serialization() {
    let ctx = NotFound::new();
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
}

#[test]
fn already_exists_serialization() {
    let ctx = AlreadyExists::new();
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
}

#[test]
fn permission_denied_serialization() {
    let ctx = PermissionDenied::new("CROSS_TENANT_ACCESS");
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
    assert_eq!(json["reason"], "CROSS_TENANT_ACCESS");
}

#[test]
fn resource_exhausted_serialization() {
    let ctx = ResourceExhausted::new(vec![QuotaViolation::new("rpm", "exceeded")]);
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json["violations"].is_array());
    assert_eq!(json["violations"][0]["subject"], "rpm");
}

#[test]
fn failed_precondition_serialization() {
    let ctx = FailedPrecondition::new(vec![PreconditionViolation::new("STATE", "s", "d")]);
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json["violations"].is_array());
}

#[test]
fn aborted_serialization() {
    let ctx = Aborted::new("OPTIMISTIC_LOCK_FAILURE");
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
    assert_eq!(json["reason"], "OPTIMISTIC_LOCK_FAILURE");
}

#[test]
fn out_of_range_field_violations_serialization() {
    let ctx = OutOfRange::new(vec![FieldViolation::new(
        "page",
        "must be between 1 and 12",
        "OUT_OF_RANGE",
    )]);
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json["field_violations"].is_array());
    assert_eq!(json["field_violations"][0]["field"], "page");
}

#[test]
fn unimplemented_serialization() {
    let ctx = Unimplemented::new();
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
}

#[test]
fn internal_serialization() {
    let ctx = Internal::new("db pool exhausted");
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
}

#[test]
fn service_unavailable_serialization() {
    let ctx = ServiceUnavailable::new(Some(30));
    let json = serde_json::to_value(&ctx).unwrap();
    assert_eq!(json, serde_json::json!({ "retry_after_seconds": 30 }));
}

#[test]
fn service_unavailable_with_reason_serialization() {
    let ctx = ServiceUnavailable::new(Some(300)).with_reason("UPSTREAM_DISABLED");
    let json = serde_json::to_value(&ctx).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "retry_after_seconds": 300, "reason": "UPSTREAM_DISABLED" })
    );
}

#[test]
fn service_unavailable_deserializes_without_reason() {
    let ctx: ServiceUnavailable = serde_json::from_str(r#"{"retry_after_seconds":30}"#).unwrap();
    assert_eq!(ctx.retry_after_seconds, Some(30));
    assert!(ctx.reason.is_none());
}

#[test]
fn data_loss_serialization() {
    let ctx = DataLoss::new();
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
}

#[test]
fn unauthenticated_serialization() {
    let ctx = Unauthenticated::new();
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.is_object());
}
