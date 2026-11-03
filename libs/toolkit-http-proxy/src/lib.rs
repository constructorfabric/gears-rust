#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

//! Upstream proxy engine for `ToolKit` gateways.
//!
//! A pooled HTTP/1.1 + HTTP/2 client built on `hyper::client::conn::http1`,
//! the `h2` crate and `tokio-rustls`, meant to sit behind a reverse proxy's
//! own request pipeline:
//!
//! - **No DNS inside the engine.** A [`Target`] carries an already resolved
//!   (and, for gateways, SSRF-filtered) socket address plus the server name
//!   used for SNI and `Host`/`:authority`. Resolution, filtering, load
//!   balancing and health checks live in [`balance`] and are composed by the
//!   caller.
//! - **TLS from `toolkit-http`.** Every `rustls::ClientConfig` is produced by
//!   `toolkit_http::{native,webpki}_roots_client_config`, so the provider
//!   selection and FIPS 140-3 hardening (`--features fips`) are shared with
//!   the rest of the platform.
//! - **ALPN with a per-host hint cache.** HTTPS targets negotiate `h2` or
//!   `http/1.1`; the negotiated protocol is remembered so later connections
//!   offer only what the host speaks.
//! - **Connection pooling.** A capped set of multiplexed connections per
//!   HTTP/2 host; a LIFO idle stack per HTTP/1.1 host, with per-host and
//!   global caps. An HTTP/1.1 connection is driven by the exchange that owns
//!   it and only returns to the pool after its response body was read to the
//!   end.
//! - **Safe retry.** A request is retried at most once, and only when the
//!   upstream provably did not process it: it never left, the peer answered
//!   `REFUSED_STREAM` or `HTTP_1_1_REQUIRED`, or, for idempotent requests
//!   only, a graceful `GOAWAY` or a stale keep-alive connection. A non-empty
//!   body is sent again only when attached as a [`ReplayableBody`].
//! - **Bounded uploads.** HTTP/2 bodies are queued only as fast as the
//!   peer's flow-control window opens; a window closed for `write_timeout`
//!   resets the stream, and a socket that stops accepting writes for
//!   `write_timeout` is severed.
//! - **Framing hygiene.** Outbound framing is always generated from the body
//!   (see [`sanitize`]); hop-by-hop, `Expect` and `HTTP2-Settings` headers
//!   never reach the upstream, and protocol upgrades are tunnelled only after
//!   the upstream answered `101 Switching Protocols`.

pub mod balance;
mod body;
mod client;
mod config;
mod connect;
mod error;
mod http2;
mod idle;
mod kill;
mod observe;
mod pool;
mod protocol_cache;
pub mod sanitize;
mod target;
mod timer;
mod tls;

pub use body::{
    BoxError, ProxyBody, ReplayableBody, ResponseBody, empty_body, full_body, stream_body,
};
pub use client::{ProxyClient, UpgradeOutcome};
pub use config::{
    CertVerification, H1Limits, H2Limits, PoolConfig, ProxyClientConfig, RootStore, TlsOptions,
};
pub use error::{ErrorKind, ProxyError};
pub use observe::{ConnectionInfo, NoopObserver, Observer};
pub use protocol_cache::CachedProtocol;
pub use target::{ProtocolPolicy, Target, Transport};
