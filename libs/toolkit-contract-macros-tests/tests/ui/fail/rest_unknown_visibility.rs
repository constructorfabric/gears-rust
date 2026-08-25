//! An unknown `visibility` value on the trait attribute must be rejected.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn get_thing(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1", visibility = "everywhere")]
pub trait DemoApiRest: DemoApi {
    #[get("/thing/{id}")]
    async fn get_thing(&self, ctx: &SecurityContext, id: String) -> Result<u32, std::io::Error>;
}

fn main() {}
