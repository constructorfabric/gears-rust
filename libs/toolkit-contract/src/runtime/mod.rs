//! Runtime helpers shared by generated REST clients.
//!
//! Gated behind the `runtime-client` feature so SDK crates can re-export the
//! contract types without pulling in `toolkit-http` / `tokio` transitively.
//!
//! ### Why a submodule, not its own crate
//!
//! The `runtime-client` feature already provides the same isolation as a
//! crate boundary would — IR-only consumers don't compile or link any of
//! the HTTP/SSE/retry plumbing. Splitting into a separate
//! `toolkit-contract-runtime` crate would add Cargo / CI ceremony without
//! changing what gets compiled in either configuration. The split is only
//! warranted if a hand-written client wants to consume the runtime
//! standalone (without `toolkit-contract`'s IR + macros). Until that
//! consumer exists, YAGNI.

pub mod transport_error;

#[cfg(feature = "canonical-errors")]
pub mod canonical;

#[cfg(feature = "runtime-client")]
pub mod client;
#[cfg(feature = "runtime-client")]
pub mod config;
#[cfg(feature = "runtime-client")]
pub mod http;
#[cfg(feature = "runtime-client")]
pub mod retry;
#[cfg(feature = "runtime-client")]
pub mod sse;
