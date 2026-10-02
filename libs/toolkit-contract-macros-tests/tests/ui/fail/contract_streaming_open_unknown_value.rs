//! An unrecognised `open = <x>` value must be a compile error, not a silent
//! fall-through (#4740). Only `fallible` and `immediate` are legal selectors.

use toolkit_contract::contract;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming(open = bogus)]
    async fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
