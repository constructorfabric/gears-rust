//! `#[toolkit::consumes]` parses a valid attribute. Without the
//! `directory-rest-client` feature (not enabled in this test crate) the
//! generated wire fn + `ConsumerRegistration` are cfg-gated out, so only the
//! inert struct remains and the crate compiles cleanly.

use toolkit_contract::consumes;

#[allow(dead_code)]
pub trait FooApi: Send + Sync {}

#[allow(dead_code)]
#[consumes(contract = FooApi, from = "bar")]
struct Consumer;

fn main() {}
