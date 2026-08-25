//! `#[toolkit::grpc_contract]` detects "security context" parameters by
//! type-name suffix (`*SecurityContext`) and excludes them from the wire
//! payload. The macro additionally emits a static assertion that the type
//! implements `SecurityContextMarker` — so accidentally naming a wire DTO
//! `SecurityContext` (without opting into the marker) must fail to compile.

use toolkit_contract::{contract, grpc_contract};

/// Locally-named struct that *isn't* a real security context. The macro
/// will skip it from the wire payload (because of the type-name match) but
/// the emitted `assert_security_context::<SecurityContext>()` will fail
/// because this struct does not impl `SecurityContextMarker`.
pub struct SecurityContext {
    pub _placeholder: u32,
}

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn ping(&self, ctx: SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

#[grpc_contract(
    package = "demo.v1",
    service = "DemoApi",
    stubs_module = "crate::stubs"
)]
pub trait DemoApiGrpc: DemoApi {
    #[rpc(name = "Ping")]
    async fn ping(&self, ctx: SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}
