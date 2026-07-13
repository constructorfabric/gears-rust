extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::Problem;
use toolkit_canonical_errors::resource_error;
use toolkit_gts::gts_id;

const USER_RESOURCE: &str = gts_id!("cf.core.users.user.v1~");
const NOT_FOUND_ERROR: &str = gts_id!("cf.core.errors.err.v1~cf.core.err.not_found.v1~");
const PERMISSION_DENIED_ERROR: &str =
    gts_id!("cf.core.errors.err.v1~cf.core.err.permission_denied.v1~");

#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct TestUserResourceError;

#[test]
fn macro_not_found_has_correct_resource_type_and_resource_info() {
    let err = TestUserResourceError::not_found("User not found")
        .with_resource("user-123")
        .create();
    assert_eq!(err.resource_type(), Some(USER_RESOURCE));
    assert_eq!(err.gts_type(), NOT_FOUND_ERROR);
    let problem = Problem::from(err);
    assert_eq!(problem.context["resource_type"], USER_RESOURCE);
    assert_eq!(problem.context["resource_name"], "user-123");
}

#[test]
fn macro_permission_denied_has_correct_resource_type() {
    let err = TestUserResourceError::permission_denied()
        .with_reason("INSUFFICIENT_ROLE")
        .create();
    assert_eq!(err.resource_type(), Some(USER_RESOURCE));
    assert_eq!(err.gts_type(), PERMISSION_DENIED_ERROR);
}

#[test]
fn problem_json_includes_resource_type_when_set() {
    let err = TestUserResourceError::not_found("User not found")
        .with_resource("user-123")
        .create();
    let problem = Problem::from(err);
    let json = serde_json::to_value(&problem).unwrap();
    assert_eq!(json["context"]["resource_type"], USER_RESOURCE);
}
