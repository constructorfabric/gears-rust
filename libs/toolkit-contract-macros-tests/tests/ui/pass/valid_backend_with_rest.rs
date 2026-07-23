//! Valid `Backend` base + `BackendRest` projection — must compile.

use toolkit_contract::{contract, rest_contract};

#[contract(gear = "billing", version = "v1")]
pub trait BillingBackend: Send + Sync {
    async fn deliver(&self, body: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/billing/v1")]
pub trait BillingBackendRest: BillingBackend {
    #[post("/deliver")]
    async fn deliver(&self, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
