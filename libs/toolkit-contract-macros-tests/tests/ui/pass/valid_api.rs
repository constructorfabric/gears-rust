//! Valid `Api` contract — must compile.

use toolkit_contract::contract;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn ping(&self) -> Result<String, std::io::Error>;
}

fn main() {}
