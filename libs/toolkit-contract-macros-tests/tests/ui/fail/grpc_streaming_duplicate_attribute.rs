//! A second `#[streaming]` attribute on one gRPC projection method must be a
//! compile error (#4740) rather than last-one-wins. gRPC framing is fixed, so
//! the two attributes below disagree on the open shape (`fallible` vs
//! `immediate`); silently keeping the last would change the emitted open shape
//! with no diagnostic.

use toolkit_contract::{contract, grpc_contract};

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming(open = fallible)]
    async fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

#[grpc_contract(
    package = "demo.v1",
    service = "DemoApi",
    stubs_module = "crate::stubs"
)]
pub trait DemoApiGrpc: DemoApi {
    #[rpc(name = "Frames")]
    #[streaming(open = fallible)]
    #[streaming(open = immediate)]
    async fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
