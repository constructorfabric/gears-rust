//! `#[streaming]`, `#[streaming(sse)]` and `#[streaming(multipart_mixed)]`
//! (#4734 C3) must all compile on a REST projection, and the first two must be
//! the same declaration.
//!
//! The `main` below is the actual assertion: it reads the generated binding IR
//! and pins each method's framing, so "the bare marker still means SSE" is
//! checked rather than merely compiled. The base trait deliberately carries the
//! *bare* `#[streaming]` for all three — a framing selector names an HTTP media
//! type and is rejected on a transport-agnostic base contract (Q12), which
//! `fail/contract_streaming_framing_arg.rs` covers.

use std::pin::Pin;

use futures_core::Stream;
use toolkit_contract::{StreamFraming, contract, rest_contract};
use toolkit_security::SecurityContext;

#[derive(Debug, thiserror::Error)]
#[error("demo error")]
pub struct DemoError;

impl From<toolkit_contract::runtime::transport_error::TransportError> for DemoError {
    fn from(_: toolkit_contract::runtime::transport_error::TransportError) -> Self {
        Self
    }
}

type Frames = Pin<Box<dyn Stream<Item = Result<u64, DemoError>> + Send + 'static>>;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[idempotency(SafeRead)]
    #[streaming]
    fn ticks(&self, ctx: SecurityContext, count: u64) -> Result<u64, DemoError>;

    #[idempotency(SafeRead)]
    #[streaming]
    fn sse_ticks(&self, ctx: SecurityContext, count: u64) -> Result<u64, DemoError>;

    #[idempotency(SafeRead)]
    #[streaming]
    fn parts(&self, ctx: SecurityContext, count: u64) -> Result<u64, DemoError>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    /// Bare marker: SSE, exactly as before a framing selector existed.
    #[get("/ticks")]
    #[streaming]
    #[server_manual]
    fn ticks(&self, ctx: SecurityContext, count: u64) -> Result<u64, DemoError>;

    /// The same thing said out loud.
    #[get("/sse-ticks")]
    #[streaming(sse)]
    #[server_manual]
    fn sse_ticks(&self, ctx: SecurityContext, count: u64) -> Result<u64, DemoError>;

    /// The second framing.
    #[get("/parts")]
    #[streaming(multipart_mixed)]
    #[server_manual]
    fn parts(&self, ctx: SecurityContext, count: u64) -> Result<u64, DemoError>;
}

/// Compiles only if the framing left every emitted signature alone: framing is
/// a wire-format choice, not a type-level one.
fn assert_signatures_are_framing_independent<T: DemoApi>(svc: &T, ctx: SecurityContext) {
    let a: Frames = svc.ticks(ctx.clone(), 1);
    let b: Frames = svc.sse_ticks(ctx.clone(), 1);
    let c: Frames = svc.parts(ctx, 1);
    drop((a, b, c));
}

fn main() {
    let binding = demo_api_rest_http_binding();
    let framing = |name: &str| {
        binding
            .find_method(name)
            .expect("method present in binding")
            .stream_framing
    };
    assert_eq!(framing("ticks"), StreamFraming::ServerSentEvents);
    assert_eq!(framing("sse_ticks"), StreamFraming::ServerSentEvents);
    assert_eq!(framing("parts"), StreamFraming::MultipartMixed);
}
