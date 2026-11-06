//! Every list entry needs a `from`, its own or the top-level one.

use toolkit_contract::consumes;

#[allow(dead_code)]
pub trait FooApi: Send + Sync {}
#[allow(dead_code)]
pub trait BarApi: Send + Sync {}

#[allow(dead_code)]
#[consumes(contract = [FooApi(from = "foo"), BarApi])]
struct Consumer;

fn main() {}
