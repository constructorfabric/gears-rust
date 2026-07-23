//! `Embedded` contracts cannot have REST projections (PRD D2/D6).

use toolkit_contract::{contract, rest_contract};

#[contract(gear = "demo", version = "v1")]
pub trait FooEmbedded: Send + Sync {
    async fn ping(&self) -> Result<String, std::io::Error>;
}

#[rest_contract(base_path = "/api/foo/v1")]
pub trait FooEmbeddedRest: FooEmbedded {
    #[post("/ping")]
    async fn ping(&self) -> Result<String, std::io::Error>;
}

fn main() {}
