//! A second `#[streaming]` attribute on one base-contract method must be a
//! compile error (#4740). The base macro previously parsed only the first
//! matching attribute and silently ignored the rest; the two attributes below
//! disagree on the open shape (`fallible` vs `immediate`), so ignoring the
//! second would hide an authoring mistake. The projections reject a duplicate
//! the same way.

use toolkit_contract::contract;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming(open = fallible)]
    #[streaming(open = immediate)]
    async fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
