//! A bare scalar query parameter must be a compile error.
//!
//! A query string deserializes as a map at the top level, so `Query<u64>` can
//! never decode — the route would 400 on every request while the OpenAPI spec
//! advertised the parameter as valid. The author has to wrap it in a struct.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn ticks(&self, ctx: &SecurityContext, count: u64) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/ticks")]
    async fn ticks(&self, ctx: &SecurityContext, count: u64) -> Result<u32, std::io::Error>;
}

fn main() {}
