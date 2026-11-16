//! A gRPC package without a `vN` segment is not version-checked, and the
//! service name constant is `package.Service`.

use toolkit_contract::{contract, grpc_contract};

#[contract(gear = "billing", version = "v3")]
pub trait BillingBackend: Send + Sync {
    async fn deliver(&self, body: String) -> Result<u32, std::io::Error>;
}

#[grpc_contract(package = "billing", service = "BillingBackend", stubs_module = "crate::stubs")]
pub trait BillingBackendGrpc: BillingBackend {
    async fn deliver(&self, body: String) -> Result<u32, std::io::Error>;
}

const _: () = assert!(BILLING_BACKEND_GRPC_SERVICE.len() == "billing.BillingBackend".len());

fn main() {
    assert_eq!(BILLING_BACKEND_GRPC_SERVICE, "billing.BillingBackend");
}
