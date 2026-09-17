//! `#[streaming] async fn` — the fallible-open shape (#4734 D2) — must
//! compile, on both the base trait and its REST projection.
//!
//! `async` on a streaming method used to be a hard error. It now selects an
//! *awaited* open: the emitted signature is
//! `async fn(..) -> Result<Pin<Box<dyn Stream<Item = Result<Item, E>>>>, E>`,
//! so a failure before the first item is a distinct outcome rather than the
//! stream's first item.
//!
//! The `assert_*` fns below are the actual assertion: each one only compiles
//! if the emitted signature has exactly the expected shape. The infallible
//! (`fn`) method is declared alongside to pin that the two shapes coexist and
//! that the historical one is unchanged.

use std::pin::Pin;

use futures_core::Stream;
use toolkit_contract::{contract, rest_contract};
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
    /// Fallible open: the awaited call yields `Result<Stream, E>`.
    #[idempotency(SafeRead)]
    #[streaming(open = fallible)]
    async fn frames(&self, ctx: SecurityContext, since: u64) -> Result<u64, DemoError>;

    /// Infallible open: the historical shape, returned synchronously.
    #[idempotency(SafeRead)]
    #[streaming]
    fn ticks(&self, ctx: SecurityContext, count: u64) -> Result<u64, DemoError>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/frames")]
    #[streaming(open = fallible)]
    #[server_manual]
    async fn frames(&self, ctx: SecurityContext, since: u64) -> Result<u64, DemoError>;

    #[get("/ticks")]
    #[streaming]
    #[server_manual]
    fn ticks(&self, ctx: SecurityContext, count: u64) -> Result<u64, DemoError>;
}

/// Compiles only if `frames` is `async` and returns `Result<Stream, DemoError>`
/// — i.e. the open is awaited and can fail before any item exists.
async fn assert_awaited_open<T: DemoApi>(svc: &T, ctx: SecurityContext) {
    let opened: Result<Frames, DemoError> = svc.frames(ctx, 7).await;
    drop(opened);
}

/// Compiles only if `ticks` is still a plain `fn` returning the bare stream.
fn assert_immediate_open<T: DemoApi>(svc: &T, ctx: SecurityContext) {
    let stream: Frames = svc.ticks(ctx, 3);
    drop(stream);
}

fn main() {}
