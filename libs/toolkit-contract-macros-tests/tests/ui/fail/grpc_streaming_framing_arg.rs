//! A framing selector on a **gRPC projection** must be a compile error
//! (#4734 C3). The framing names an HTTP media type; gRPC has none, so
//! accepting the argument would silently ignore it.

use toolkit_contract::{contract, grpc_contract};

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming]
    fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

#[grpc_contract(
    package = "demo.v1",
    service = "DemoApi",
    stubs_module = "crate::stubs"
)]
pub trait DemoApiGrpc: DemoApi {
    #[rpc(name = "Frames")]
    #[streaming(multipart_mixed)]
    fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
