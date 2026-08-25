//! `#[retryable]` on a `NotIdempotent` RPC must be a compile error.
//!
//! The generated client retries transient transport failures, and a 502/504 can
//! arrive after the server has already committed — so retrying a
//! non-idempotent write duplicates it. The two attributes used to be
//! independent flags with nothing reconciling them.

use toolkit_contract::{contract, grpc_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    async fn charge(&self, ctx: SecurityContext, amount: u64) -> Result<u64, std::io::Error>;
}

#[grpc_contract(
    package = "demo.v1",
    service = "DemoApi",
    stubs_module = "crate::stubs"
)]
pub trait DemoApiGrpc: DemoApi {
    #[rpc(name = "Charge")]
    #[idempotency_level(NotIdempotent)]
    #[retryable]
    async fn charge(&self, ctx: SecurityContext, amount: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
