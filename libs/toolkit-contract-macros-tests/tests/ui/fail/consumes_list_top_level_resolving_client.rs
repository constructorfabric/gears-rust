//! With several contracts, `resolving_client` goes on the list entry.

use toolkit_contract::consumes;

#[allow(dead_code)]
pub trait FooApi: Send + Sync {}
#[allow(dead_code)]
pub trait BarApi: Send + Sync {}

#[allow(dead_code)]
#[consumes(from = "bar", resolving_client = FooClient, contract = [FooApi, BarApi])]
struct Consumer;

fn main() {}
