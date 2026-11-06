//! With several contracts, `local` goes on the list entry.

use toolkit_contract::provides;

#[allow(dead_code)]
pub trait FooApi: Send + Sync {}
#[allow(dead_code)]
pub trait BarApi: Send + Sync {}

#[allow(dead_code)]
#[provides(local = make, contract = [FooApi, BarApi])]
struct Provider;

fn main() {}
