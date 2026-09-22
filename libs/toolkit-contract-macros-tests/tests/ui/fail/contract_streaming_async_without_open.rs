//! `async fn` on a base streaming method with no explicit `open = fallible` is
//! a compile error (#4734). The open shape is selected by
//! `#[streaming(open = fallible)]`, not by the `async` keyword — so an `async`
//! that would silently pick the fallible open is rejected, and a stray `async`
//! (or a deleted one) can no longer change the generated client API unnoticed.

use toolkit_contract::contract;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming]
    async fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
