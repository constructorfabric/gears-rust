//! Two methods declaring the same `#[rpc(name = "X")]` — must fail.

use toolkit_contract::{contract, grpc_contract};

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn one(&self, body: String) -> Result<u32, std::io::Error>;
    async fn two(&self, body: String) -> Result<u32, std::io::Error>;
}

#[grpc_contract(
    package = "demo.v1",
    service = "DemoApi",
    stubs_module = "crate::stubs"
)]
pub trait DemoApiGrpc: DemoApi {
    #[rpc(name = "Dup")]
    async fn one(&self, body: String) -> Result<u32, std::io::Error>;

    #[rpc(name = "Dup")]
    async fn two(&self, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
