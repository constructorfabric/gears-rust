extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::resource_error;

#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct UserResourceError;

fn main() {
    // failed_precondition requires at least one .with_precondition_violation() before .create()
    let _err = UserResourceError::failed_precondition().create();
}
