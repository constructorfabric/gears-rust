//! gRPC projection cannot extend an `Embedded` (always-local) contract —
//! must fail per PRD #1536 D2/D6.

use toolkit_contract::{contract, grpc_contract};

#[contract(gear = "demo", version = "v1")]
pub trait FooEmbedded: Send + Sync {
    async fn ping(&self) -> Result<String, std::io::Error>;
}

#[grpc_contract(
    package = "demo.foo.v1",
    service = "FooEmbedded",
    stubs_module = "crate::stubs"
)]
pub trait FooEmbeddedGrpc: FooEmbedded {
    #[rpc(name = "Ping")]
    async fn ping(&self) -> Result<String, std::io::Error>;
}

fn main() {}
