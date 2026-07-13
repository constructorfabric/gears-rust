extern crate toolkit_canonical_errors;

use toolkit_canonical_errors::resource_error;

#[resource_error(gts_id!("cf.core.users.user.v1~"))]
struct UserResourceError;

fn main() {
    // aborted requires .with_reason() before .create()
    let _err = UserResourceError::aborted("Operation aborted due to concurrency conflict").create();
}
