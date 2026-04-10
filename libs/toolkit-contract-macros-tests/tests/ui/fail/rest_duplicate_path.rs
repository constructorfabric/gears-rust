//! Two methods declaring the same `(verb, path)` pair — must fail.

use toolkit_contract::{contract, rest_contract};

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn one(&self, body: String) -> Result<u32, std::io::Error>;
    async fn two(&self, body: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[post("/dup")]
    async fn one(&self, body: String) -> Result<u32, std::io::Error>;

    #[post("/dup")]
    async fn two(&self, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
