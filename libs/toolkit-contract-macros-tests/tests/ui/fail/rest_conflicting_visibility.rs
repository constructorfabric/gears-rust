//! `#[exposed]` and `#[internal]` on the same method must be a compile error.
//!
//! Letting one silently win would change what the gateway reverse-proxies with
//! no signal — the two markers exist precisely so a trait-level `visibility`
//! default can be overridden in either direction.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn get_thing(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/thing/{id}")]
    #[exposed]
    #[internal]
    async fn get_thing(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

fn main() {}
