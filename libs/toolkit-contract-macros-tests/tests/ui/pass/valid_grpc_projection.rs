//! Valid `Backend` base + `BackendGrpc` projection — must compile (the
//! tonic stubs aren't required at this point because the generated client
//! struct is gated on the `grpc-client` feature).

use toolkit_contract::{contract, grpc_contract};

#[contract(gear = "billing", version = "v1")]
pub trait BillingBackend: Send + Sync {
    async fn deliver(&self, body: String) -> Result<u32, std::io::Error>;
}

#[grpc_contract(
    package = "billing.backend.v1",
    service = "BillingBackend",
    stubs_module = "crate::stubs"
)]
pub trait BillingBackendGrpc: BillingBackend {
    #[rpc(name = "Deliver")]
    async fn deliver(&self, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
