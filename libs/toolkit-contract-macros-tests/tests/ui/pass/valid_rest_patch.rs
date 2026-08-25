//! `#[patch(...)]` is a supported verb (M-6): a PATCH projection method with a
//! path param and a JSON body must expand cleanly, exactly like POST/PUT.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "billing", version = "v1")]
pub trait BillingBackend: Send + Sync {
    async fn update_item(
        &self,
        ctx: &SecurityContext,
        id: String,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/billing/v1")]
pub trait BillingBackendRest: BillingBackend {
    #[patch("/items/{id}")]
    async fn update_item(
        &self,
        ctx: &SecurityContext,
        id: String,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

fn main() {}
