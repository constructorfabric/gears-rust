//! `#[streaming] async fn` on a **gRPC projection** compiles, and emits the
//! fallible-open signature (#4734, amending Q1).
//!
//! This was a hard error through Phase B, on the reasoning that a fallible open
//! was REST-only. It is not: tonic's client call is already
//! `async fn(..) -> Result<Response<Streaming<T>>, Status>` and its server trait
//! method is already `async fn(..) -> Result<Response<Self::Stream>, Status>`,
//! so the open and the messages are two distinct phases on the gRPC wire
//! whether or not the contract says so. `#[streaming] fn` flattens them, and an
//! open-time `Status` arrives as the stream's first item; `#[streaming] async
//! fn` keeps them apart.
//!
//! Both shapes are declared below so the `assert_*` fns pin each emitted
//! signature by construction, and so the historical one is pinned as unchanged.

use std::pin::Pin;

use futures_core::Stream;
use toolkit_contract::{contract, grpc_contract};

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
    #[streaming(open = fallible)]
    async fn frames(&self, since: u64) -> Result<u64, DemoError>;

    /// Infallible open: the historical shape, returned synchronously.
    #[streaming]
    fn ticks(&self, count: u64) -> Result<u64, DemoError>;
}

#[grpc_contract(
    package = "demo.v1",
    service = "DemoApi",
    stubs_module = "crate::stubs"
)]
pub trait DemoApiGrpc: DemoApi {
    #[rpc(name = "Frames")]
    #[streaming(open = fallible)]
    async fn frames(&self, since: u64) -> Result<u64, DemoError>;

    #[rpc(name = "Ticks")]
    #[streaming]
    fn ticks(&self, count: u64) -> Result<u64, DemoError>;
}

/// Compiles only if `frames` is `async` and returns `Result<Stream, DemoError>`
/// — i.e. the open is awaited and can fail before any message exists.
async fn assert_awaited_open<T: DemoApi>(svc: &T) {
    let opened: Result<Frames, DemoError> = svc.frames(7).await;
    drop(opened);
}

/// Compiles only if `ticks` is still a plain `fn` returning the bare stream.
fn assert_immediate_open<T: DemoApi>(svc: &T) {
    let stream: Frames = svc.ticks(3);
    drop(stream);
}

fn main() {}
