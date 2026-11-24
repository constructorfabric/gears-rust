extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::resource_error;

#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct UserResourceError;

fn main() {
    // with_reason is offered on deadline_exceeded only, not on other
    // categories without a context
    let _err = UserResourceError::unimplemented("Not implemented")
        .with_reason("REQUEST_NOT_SENT")
        .create();
}
