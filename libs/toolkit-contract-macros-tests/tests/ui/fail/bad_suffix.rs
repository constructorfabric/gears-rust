//! Trait name does not end with `Api`/`Embedded`/`Backend`/`Extension` —
//! must fail per PRD #1536 D6 hard rule.

use toolkit_contract::contract;

#[contract(gear = "demo", version = "v1")]
pub trait DemoService: Send + Sync {
    async fn ping(&self) -> Result<String, std::io::Error>;
}

fn main() {}
