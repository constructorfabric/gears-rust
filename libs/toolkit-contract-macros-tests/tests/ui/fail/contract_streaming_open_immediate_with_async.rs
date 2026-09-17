//! `#[streaming(open = immediate)]` on an `async fn` is a compile error
//! (#4734): the explicit selector and the `async` keyword contradict each
//! other. The author must remove `async` for the immediate shape, or write
//! `open = fallible` for the awaited fallible open.

use toolkit_contract::contract;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming(open = immediate)]
    async fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
