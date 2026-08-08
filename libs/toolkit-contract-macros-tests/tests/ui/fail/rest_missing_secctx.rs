//! A remote projection method WITHOUT a security-plane context as its first
//! argument must fail (DESIGN §2.2) — otherwise it would dispatch
//! unauthenticated with no compile-time signal.

use toolkit_contract::{contract, rest_contract};

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn charge(&self, body: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[post("/charge")]
    async fn charge(&self, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
