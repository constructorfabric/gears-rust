extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::resource_error;

#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct UserResourceError;

fn main() {
    // invalid_argument requires at least one .with_field_violation() before .create()
    let _err = UserResourceError::invalid_argument().create();
}
