//! Base↔projection signature parity (ADR-0003): a projection method that
//! redeclares a base method with a DIFFERENT parameter type must fail to
//! compile. The macro rewrites the projection method into a default body that
//! delegates via `<Self as Base>::method(..)`, so the drifted `body: u64`
//! argument fails to type-check against the base's `body: String`.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn charge(&self, ctx: &SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    // Param type drifts from the base (`String` -> `u64`).
    #[post("/charge")]
    async fn charge(&self, ctx: &SecurityContext, body: u64) -> Result<u32, std::io::Error>;
}

fn main() {}
