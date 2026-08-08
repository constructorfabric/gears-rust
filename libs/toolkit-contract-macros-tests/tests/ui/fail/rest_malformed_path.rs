//! A malformed path template (unterminated `{`) must be a compile error at the
//! attribute span (M-12), not a silently-broken route surfaced later.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn get_thing(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/thing/{id")]
    async fn get_thing(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

fn main() {}
