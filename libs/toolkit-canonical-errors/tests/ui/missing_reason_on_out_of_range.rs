extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::resource_error;

#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct UserResourceError;

fn main() {
    // out_of_range requires .with_field_violation() before .create()
    let _err = UserResourceError::out_of_range("Page out of range").create();
}
