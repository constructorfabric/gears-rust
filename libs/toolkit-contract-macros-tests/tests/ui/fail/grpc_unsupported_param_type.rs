//! `#[toolkit::grpc_contract]` must reject method parameters whose type does
//! not implement `GrpcRepr`. `i128` is the canonical "no proto3 equivalent"
//! primitive — proto3 has no 128-bit integer type, so we don't impl
//! `GrpcRepr` for it. The compile error must come from the static guard
//! emitted by the macro.

use toolkit_contract::{contract, grpc_contract};

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn beam(&self, big: i128) -> Result<u32, std::io::Error>;
}

#[grpc_contract(
    package = "demo.v1",
    service = "DemoApi",
    stubs_module = "crate::stubs"
)]
pub trait DemoApiGrpc: DemoApi {
    #[rpc(name = "Beam")]
    async fn beam(&self, big: i128) -> Result<u32, std::io::Error>;
}

fn main() {}
