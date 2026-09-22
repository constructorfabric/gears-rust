//! A framing selector on the **base** contract trait must be a compile error
//! (#4734 Q12).
//!
//! The base contract is transport-agnostic — it is projected to REST, to gRPC,
//! or to neither — so it carries no HTTP media type for a framing to select.
//! The base macro matches `#[streaming]` on its path alone, so without an
//! explicit rejection the argument would be accepted and silently discarded,
//! and an author who wrote the framing only here would get SSE on the wire with
//! no diagnostic. `#[grpc_contract]` refuses it for the same reason.

use toolkit_contract::contract;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming(multipart_mixed)]
    fn frames(&self, since: u64) -> Result<u64, std::io::Error>;
}

fn main() {}
