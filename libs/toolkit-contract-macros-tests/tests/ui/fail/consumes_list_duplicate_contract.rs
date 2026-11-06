//! A contract listed twice would wire one proxy for both entries.

use toolkit_contract::consumes;

#[allow(dead_code)]
pub trait FooApi: Send + Sync {}

#[allow(dead_code)]
#[consumes(from = "bar", contract = [FooApi, FooApi(from = "baz")])]
struct Consumer;

fn main() {}
