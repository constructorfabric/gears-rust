//! `#[toolkit::consumes]` rejects unknown arguments.

use toolkit_contract::consumes;

#[allow(dead_code)]
pub trait FooApi: Send + Sync {}

#[allow(dead_code)]
#[consumes(contract = FooApi, from = "bar", bogus = 1)]
struct Consumer;

fn main() {}
