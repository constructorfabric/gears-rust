//! Regression coverage for `#[toolkit::provides]`'s per-transport method
//! shape (`toolkit-contract-macros/src/provides.rs::build_wire_method_item`).
//!
//! The generated `wire_<contract>` body only ever awaits inside the gRPC
//! arm (`#grpc_client_path::connect(...).await`), and that arm is only
//! emitted when `grpc` is one of the declared `transports`. Emitting
//! `pub async fn` unconditionally therefore used to hand a `transports =
//! [local]` (or `[local, rest]`) gear an `async fn` whose body never
//! awaited anything, and `clippy::unused_async_trait_impl` (new in Rust
//! 1.98.0) fired on the caller's own `#[toolkit::provides(...)]` attribute
//! — a finding the gear author has no way to fix, since the body is
//! entirely macro-generated.
//!
//! This crate carries `[lints] workspace = true` (`Cargo.toml`), so
//! `cargo clippy -p cf-gears-toolkit-contract-macros-tests --all-targets
//! --all-features -- -D warnings` re-lints the code below under the same
//! bar as every other crate: if a future refactor makes the non-gRPC path
//! `async fn` again, that clippy run fails here instead of on every
//! downstream gear's CI.

#![allow(clippy::unwrap_used, clippy::unnecessary_wraps)]

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, GearCtx};
use toolkit_contract::policy::PolicyStack;
use toolkit_contract::runtime::config::ClientConfig;
use uuid::Uuid;

#[toolkit::contract(gear = "provides-transport-shape", version = "v1")]
pub trait EchoApi: Send + Sync {
    async fn echo(&self, body: String) -> Result<String, std::io::Error>;
}

struct EchoLocal;

#[async_trait::async_trait]
impl EchoApi for EchoLocal {
    async fn echo(&self, body: String) -> Result<String, std::io::Error> {
        Ok(body)
    }
}

/// `transports = [local]` — no `grpc` arm at all, the exact shape that used
/// to trigger the false `unused_async_trait_impl` finding.
#[toolkit::provides(
    contract   = self::EchoApi,
    local      = LocalOnlyProvider::build_local,
    transports = [local],
)]
#[derive(Default)]
pub struct LocalOnlyProvider;

impl LocalOnlyProvider {
    // `#[toolkit::provides]` applies `?` to this factory's return value, so
    // `Result` is part of the macro's calling convention, not incidental.
    fn build_local(
        _ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn EchoApi>> {
        Ok(Arc::new(EchoLocal))
    }
}

/// Stand-in gRPC client: only `connect` matters here, the RPC is never
/// invoked (wiring falls back to `Local` by default in the test harness
/// below).
pub struct EchoGrpcClient;

#[async_trait::async_trait]
impl EchoApi for EchoGrpcClient {
    async fn echo(&self, body: String) -> Result<String, std::io::Error> {
        Ok(body)
    }
}

impl EchoGrpcClient {
    // No real connect work happens here (there is no server), so this must
    // not be `async fn` either — same lesson as the macro fix under test.
    fn connect(_cfg: ClientConfig) -> impl std::future::Future<Output = anyhow::Result<Self>> {
        std::future::ready(Ok(Self))
    }
}

/// `transports = [local, grpc]` — the gRPC arm keeps the public method a
/// genuine `async fn` that awaits `EchoGrpcClient::connect(...)`, proving
/// this fix didn't collapse that branch to the non-async shape too.
#[toolkit::provides(
    contract    = self::EchoApi,
    local       = GrpcCapableProvider::build_local,
    grpc_client = self::EchoGrpcClient,
    transports  = [local, grpc],
)]
#[derive(Default)]
pub struct GrpcCapableProvider;

impl GrpcCapableProvider {
    fn build_local(
        _ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn EchoApi>> {
        Ok(Arc::new(EchoLocal))
    }
}

struct StaticProvider(HashMap<String, serde_json::Value>);

impl ConfigProvider for StaticProvider {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        self.0.get(gear_name)
    }
}

fn empty_ctx(gear_name: &str) -> GearCtx {
    GearCtx::new(
        gear_name,
        Uuid::nil(),
        Arc::new(StaticProvider(HashMap::new())),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn local_only_provider_wires_without_grpc() {
    let ctx = empty_ctx("provides-transport-shape");
    let provider = LocalOnlyProvider;

    provider.wire_echo_api(&ctx).await.unwrap();

    let echo = ctx.client_hub().get::<dyn EchoApi>().unwrap();
    assert_eq!(echo.echo("hi".to_owned()).await.unwrap(), "hi");
}

#[tokio::test]
async fn grpc_capable_provider_still_wires_and_awaits() {
    let ctx = empty_ctx("provides-transport-shape");
    let provider = GrpcCapableProvider;

    // No `client_wiring` config present, so this defaults to `Local` — the
    // point of this test is that the generated method still compiles as
    // `async fn` and is `.await`-able, not that the gRPC arm actually runs.
    provider.wire_echo_api(&ctx).await.unwrap();

    let echo = ctx.client_hub().get::<dyn EchoApi>().unwrap();
    assert_eq!(echo.echo("hi".to_owned()).await.unwrap(), "hi");
}
