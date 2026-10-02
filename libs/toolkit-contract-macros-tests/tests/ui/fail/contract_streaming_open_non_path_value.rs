//! `open = "fallible"` (a string literal, not a bare name) must be a compile
//! error (#4740). The selector is a bare identifier, matching the framing and
//! idempotency argument style.

use toolkit_contract::contract;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming(open = "fallible")]
    async fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
