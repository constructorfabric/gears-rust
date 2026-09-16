#![allow(clippy::expect_used)]
use cel_core::{CelType, Env};
#[test]
fn dependency_parses_macros_with_locations_and_checks_scalar_types() {
    let env = Env::with_standard_library().with_variable("amount", CelType::Int);
    let checked = env
        .compile("[1, 2].map(x, x + amount)")
        .expect("typed macro");
    assert!(checked.is_checked());
    assert!(env.compile("amount + true").is_err());
    assert!(env.compile("principal.tenant").is_err());
    assert!(env.parse_only("[1, 2").is_err());
}
