//! Base↔projection coverage (ADR-0003): a projection method that does NOT
//! exist on the base trait must fail to compile — the macro-synthesized default
//! body delegates via `<Self as Base>::extra(..)`, and no such base method
//! exists, so it cannot generate unreachable client dispatch.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn charge(&self, ctx: &SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[post("/charge")]
    async fn charge(&self, ctx: &SecurityContext, body: String) -> Result<u32, std::io::Error>;

    // `extra` is absent from the base `DemoApi` — must not compile.
    #[get("/extra")]
    async fn extra(&self, ctx: &SecurityContext) -> Result<u32, std::io::Error>;
}

fn main() {}
