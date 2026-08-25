//! A `{param}` in the path template with no matching method parameter must be a
//! compile error.
//!
//! The generated client substitutes placeholders by name at runtime, so a typo
//! previously compiled fine and shipped a URL with a literal `{tenant_id}` in
//! it. The runtime IR validator catches it, but only once a consumer wires the
//! contract — far too late.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn get_thing(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/tenants/{tenant_id}/thing/{id}")]
    async fn get_thing(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

fn main() {}
