//! `#[toolkit::consumes]` takes `transport = rest` or `transport = grpc` only.

use toolkit_contract::consumes;

#[allow(dead_code)]
pub trait FooApi: Send + Sync {}

#[allow(dead_code)]
#[consumes(contract = FooApi, from = "bar", transport = soap)]
struct Consumer;

fn main() {}
