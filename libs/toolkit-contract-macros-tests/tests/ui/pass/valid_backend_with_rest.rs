//! Valid `Backend` base + `BackendRest` projection — must compile.
//!
//! Every remote method carries a tenant `SecurityContext` as its first
//! argument, per DESIGN §2.2.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "billing", version = "v1")]
pub trait BillingBackend: Send + Sync {
    async fn deliver(&self, ctx: SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/billing/v1")]
pub trait BillingBackendRest: BillingBackend {
    #[post("/deliver")]
    async fn deliver(&self, ctx: SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
