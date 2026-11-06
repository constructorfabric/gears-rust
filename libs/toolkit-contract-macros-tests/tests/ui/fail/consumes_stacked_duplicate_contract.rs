//! Two `#[consumes]` of one contract on a struct would share one client.

use toolkit_contract::consumes;

#[allow(dead_code)]
pub trait FooApi: Send + Sync {}

#[allow(dead_code)]
#[consumes(contract = FooApi, from = "bar")]
#[consumes(contract = FooApi, from = "baz")]
struct Consumer;

fn main() {}
