//! The DESIGN §2.2 reference form `&SecurityContext` must be accepted by the
//! REST projection macro (reference-transparent detection) — it is excluded
//! from the wire payload and used as the auth carrier, exactly like the
//! by-value form.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "billing", version = "v1")]
pub trait BillingBackend: Send + Sync {
    async fn get_item(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/billing/v1")]
pub trait BillingBackendRest: BillingBackend {
    #[get("/items/{id}")]
    async fn get_item(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

fn main() {}
