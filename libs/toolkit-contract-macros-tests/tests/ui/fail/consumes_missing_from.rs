//! `#[toolkit::consumes]` requires the `from = "gear"` argument.

use toolkit_contract::consumes;

#[allow(dead_code)]
pub trait FooApi: Send + Sync {}

#[allow(dead_code)]
#[consumes(contract = FooApi)]
struct Consumer;

fn main() {}
