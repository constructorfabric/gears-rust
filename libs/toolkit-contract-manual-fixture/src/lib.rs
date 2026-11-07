//! Fixture SDK for `#[client_manual]` REST methods (ADR-0009).
//!
//! Two contracts:
//! - [`ManualDemoApi`] mixes a generated method with manual unary, streaming and
//!   passthrough methods, and names a default handler, so its clients keep
//!   `new`.
//! - [`StatefulDemoApi`] has no default handler: its clients are built only
//!   with `with_manual`.
//!
//! Handlers live in [`handlers`]: the `{Projection}Manual` traits are sealed to
//! this crate, so the integration tests use these exported ones. Every method
//! is `#[server_manual]`: only the clients are under test here.

use std::pin::Pin;

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

#[cfg(feature = "rest-client")]
pub mod handlers;

/// Body of the passthrough method. The same type as
/// `toolkit_contract::runtime::raw::RawBody`, named here so the contract
/// compiles without the `rest-client` feature.
pub type RawBody = http_body_util::combinators::UnsyncBoxBody<
    bytes::Bytes,
    Box<dyn std::error::Error + Send + Sync>,
>;

/// Boxed stream returned by the streaming methods.
pub type DemoStream<T> = Pin<Box<dyn Stream<Item = Result<T, CanonicalError>> + Send + 'static>>;

/// What the test server saw, echoed back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Echo {
    /// The value the call carried.
    pub value: String,
    /// `Authorization` header received by the server.
    pub authorization: Option<String>,
    /// `X-ToolKit-Internal-Token` header received by the server.
    pub internal_token: Option<String>,
    /// `X-Manual` header, set only by the manual handlers.
    pub marker: Option<String>,
}

/// Mixed contract: generated, manual and passthrough methods.
#[toolkit::contract(gear = "manual-demo", version = "v1")]
pub trait ManualDemoApi: Send + Sync {
    /// Generated dispatch.
    async fn echo(&self, ctx: SecurityContext, value: String) -> Result<Echo, CanonicalError>;

    /// Manual, tenant plane.
    async fn lookup(&self, ctx: SecurityContext, id: String) -> Result<Echo, CanonicalError>;

    /// Manual, platform plane.
    async fn audit(&self, ctx: PlatformSecurityContext, id: String)
    -> Result<Echo, CanonicalError>;

    /// Manual, stream returned immediately.
    #[streaming]
    fn ticks(&self, ctx: SecurityContext, count: u32) -> Result<u32, CanonicalError>;

    /// Manual, stream behind a fallible open.
    #[streaming(open = fallible)]
    async fn feed(&self, ctx: SecurityContext, count: u32) -> Result<u32, CanonicalError>;

    /// Manual passthrough of an opaque request.
    async fn proxy(
        &self,
        ctx: SecurityContext,
        req: http::Request<RawBody>,
    ) -> Result<http::Response<RawBody>, CanonicalError>;

    /// Manual passthrough on the platform plane.
    async fn platform_proxy(
        &self,
        ctx: PlatformSecurityContext,
        req: http::Request<RawBody>,
    ) -> Result<http::Response<RawBody>, CanonicalError>;
}

/// REST projection of [`ManualDemoApi`].
#[toolkit::rest_contract(
    base_path = "/manual-demo/v1",
    manual_handler = crate::handlers::DemoHandler
)]
pub trait ManualDemoApiRest: ManualDemoApi {
    #[post("/echo")]
    #[server_manual]
    async fn echo(&self, ctx: SecurityContext, value: String) -> Result<Echo, CanonicalError>;

    #[get("/items/{id}")]
    #[client_manual]
    #[server_manual]
    async fn lookup(&self, ctx: SecurityContext, id: String) -> Result<Echo, CanonicalError>;

    #[get("/audit/{id}")]
    #[client_manual]
    #[server_manual]
    async fn audit(&self, ctx: PlatformSecurityContext, id: String)
    -> Result<Echo, CanonicalError>;

    #[get("/ticks")]
    #[streaming]
    #[server_manual]
    #[client_manual]
    fn ticks(&self, ctx: SecurityContext, count: u32) -> Result<u32, CanonicalError>;

    #[get("/feed")]
    #[streaming(open = fallible)]
    #[server_manual]
    #[client_manual]
    async fn feed(&self, ctx: SecurityContext, count: u32) -> Result<u32, CanonicalError>;

    #[passthrough("/proxy/{*path}", methods = [get, post, put, delete])]
    #[client_manual]
    #[server_manual]
    async fn proxy(
        &self,
        ctx: SecurityContext,
        req: http::Request<RawBody>,
    ) -> Result<http::Response<RawBody>, CanonicalError>;

    #[passthrough("/platform-proxy/{*path}", methods = [get])]
    #[client_manual]
    #[server_manual]
    async fn platform_proxy(
        &self,
        ctx: PlatformSecurityContext,
        req: http::Request<RawBody>,
    ) -> Result<http::Response<RawBody>, CanonicalError>;
}

/// Contract whose manual handler carries state and has no default.
#[toolkit::contract(gear = "stateful-demo", version = "v1")]
pub trait StatefulDemoApi: Send + Sync {
    /// Manual: returns the handler's call count and the endpoint it saw.
    async fn count(&self, ctx: SecurityContext) -> Result<Echo, CanonicalError>;
}

/// REST projection of [`StatefulDemoApi`].
#[toolkit::rest_contract(base_path = "/stateful-demo/v1")]
pub trait StatefulDemoApiRest: StatefulDemoApi {
    #[get("/count")]
    #[client_manual]
    #[server_manual]
    async fn count(&self, ctx: SecurityContext) -> Result<Echo, CanonicalError>;
}
