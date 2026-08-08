//! Projection method without one of `#[get]`/`#[post]`/`#[put]`/`#[delete]`
//! — must fail.

use toolkit_contract::{contract, rest_contract};

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn naked(&self, body: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    async fn naked(&self, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
