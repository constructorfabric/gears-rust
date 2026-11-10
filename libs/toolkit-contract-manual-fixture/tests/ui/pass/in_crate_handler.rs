//! The crate that declares a projection implements its sealed handler trait,
//! including borrowed arguments and both stream-open shapes.

use std::pin::Pin;

use futures_core::Stream;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::manual::ManualCallContext;
use toolkit_security::SecurityContext;

type Items = Pin<Box<dyn Stream<Item = Result<u32, CanonicalError>> + Send + 'static>>;

#[toolkit::contract(gear = "local", version = "v1")]
pub trait LocalApi: Send + Sync {
    async fn find(&self, ctx: &SecurityContext, name: &str) -> Result<u32, CanonicalError>;

    #[streaming]
    fn now(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;

    #[streaming(open = fallible)]
    async fn later(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;
}

#[toolkit::rest_contract(base_path = "/local/v1", manual_handler = Local)]
pub trait LocalApiRest: LocalApi {
    #[get("/find")]
    #[client_manual]
    #[server_manual]
    async fn find(&self, ctx: &SecurityContext, name: &str) -> Result<u32, CanonicalError>;

    #[get("/now")]
    #[streaming]
    #[client_manual]
    #[server_manual]
    fn now(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;

    #[get("/later")]
    #[streaming(open = fallible)]
    #[client_manual]
    #[server_manual]
    async fn later(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;
}

#[derive(Default)]
pub struct Local;

impl local_api_rest_manual_sealed::Sealed for Local {}

#[async_trait::async_trait]
impl LocalApiRestManual for Local {
    async fn find(
        &self,
        _call: &ManualCallContext<'_>,
        _ctx: &SecurityContext,
        name: &str,
    ) -> Result<u32, CanonicalError> {
        Ok(u32::try_from(name.len()).unwrap_or(u32::MAX))
    }

    fn now(&self, _call: &ManualCallContext<'_>, _ctx: SecurityContext) -> Items {
        Box::pin(futures_util::stream::empty())
    }

    async fn later(
        &self,
        _call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
    ) -> Result<Items, CanonicalError> {
        Ok(Box::pin(futures_util::stream::empty()))
    }
}

// Compile-only: building a client needs a Tokio runtime.
fn main() {}

#[allow(dead_code)]
fn usage() {
    let _ = LocalApiRestClient::new(toolkit_contract::runtime::config::ClientConfig::new("http://x"));
}
