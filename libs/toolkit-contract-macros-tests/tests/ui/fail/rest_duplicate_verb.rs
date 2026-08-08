//! Two HTTP verb annotations on one projection method must be a compile error
//! (M-11) — silently letting the last one win would change the wire contract
//! with no signal.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn act(&self, ctx: &SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/act")]
    #[post("/act")]
    async fn act(&self, ctx: &SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
