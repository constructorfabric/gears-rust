//! A repeated `open` selector must be a compile error (#4740) rather than
//! last-one-wins, which would let a contradictory pair silently pick a shape.

use toolkit_contract::contract;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming(open = fallible, open = immediate)]
    async fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
