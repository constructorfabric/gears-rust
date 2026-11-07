//! A `vN` gRPC package must name the contract's major version: `billing.v2`
//! for a `version = "v1"` contract is a compile error.

use toolkit_contract::{contract, grpc_contract};

#[contract(gear = "billing", version = "v1")]
pub trait BillingBackend: Send + Sync {
    async fn deliver(&self, body: String) -> Result<u32, std::io::Error>;
}

#[grpc_contract(package = "billing.v2", service = "BillingBackend", stubs_module = "crate::stubs")]
pub trait BillingBackendGrpc: BillingBackend {
    async fn deliver(&self, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
