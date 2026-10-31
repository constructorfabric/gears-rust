//! The SDK's `{Projection}Manual` implementations.

use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::StreamExt as _;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::manual::ManualCallContext;
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

use crate::{DemoStream, Echo, RawBody};

/// Default handler of [`crate::ManualDemoApiRest`]: tags its requests with
/// `X-Manual: 1`.
#[derive(Debug, Default)]
pub struct DemoHandler;

impl crate::manual_demo_api_rest_manual_sealed::Sealed for DemoHandler {}

impl DemoHandler {
    async fn get_echo(call: &ManualCallContext<'_>, id: &str) -> Result<Echo, CanonicalError> {
        let url = call.url(&serde_json::json!({ "id": id }), None)?;
        let builder = call.request(&url)?.header("x-manual", "1");
        Ok(call.send_json(builder).await?)
    }

    /// The `count` items as a stream that owns everything it polls.
    fn numbers(
        call: &ManualCallContext<'_>,
        count: u32,
    ) -> Result<DemoStream<u32>, TransportError> {
        let url = call.url(&serde_json::json!({}), Some(&format!("count={count}")))?;
        let builder = call.request(&url)?.header("x-manual", "1");
        let timeout = call.config().timeout;
        let items = async move {
            toolkit_contract::runtime::client::send_unary::<_, Vec<u32>>(
                || Ok(builder),
                Some(timeout),
            )
            .await
        };
        Ok(Box::pin(futures_util::stream::once(items).flat_map(
            |result| match result {
                Ok(items) => futures_util::stream::iter(items.into_iter().map(Ok)).boxed(),
                Err(e) => {
                    futures_util::stream::once(async move { Err(CanonicalError::from(e)) }).boxed()
                }
            },
        )))
    }
}

#[async_trait::async_trait]
impl crate::ManualDemoApiRestManual for DemoHandler {
    async fn lookup(
        &self,
        call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
        id: String,
    ) -> Result<Echo, CanonicalError> {
        Self::get_echo(call, &id).await
    }

    async fn audit(
        &self,
        call: &ManualCallContext<'_>,
        _ctx: PlatformSecurityContext,
        id: String,
    ) -> Result<Echo, CanonicalError> {
        Self::get_echo(call, &id).await
    }

    fn ticks(
        &self,
        call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
        count: u32,
    ) -> DemoStream<u32> {
        Self::numbers(call, count).unwrap_or_else(|e| {
            Box::pin(futures_util::stream::once(async move {
                Err(CanonicalError::from(e))
            }))
        })
    }

    async fn feed(
        &self,
        call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
        count: u32,
    ) -> Result<DemoStream<u32>, CanonicalError> {
        Ok(Self::numbers(call, count)?)
    }

    async fn proxy(
        &self,
        call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
        req: http::Request<RawBody>,
    ) -> Result<http::Response<RawBody>, CanonicalError> {
        Ok(call.send_raw(req).await?)
    }

    async fn platform_proxy(
        &self,
        call: &ManualCallContext<'_>,
        _ctx: PlatformSecurityContext,
        req: http::Request<RawBody>,
    ) -> Result<http::Response<RawBody>, CanonicalError> {
        Ok(call.send_raw(req).await?)
    }
}

/// Handler of [`crate::StatefulDemoApiRest`] that counts its calls. The
/// projection names no default handler, so clients are built with
/// `with_manual`.
#[derive(Debug, Default)]
pub struct StatefulHandler {
    calls: AtomicUsize,
}

impl StatefulHandler {
    /// Calls handled so far.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl crate::stateful_demo_api_rest_manual_sealed::Sealed for StatefulHandler {}

#[async_trait::async_trait]
impl crate::StatefulDemoApiRestManual for StatefulHandler {
    async fn count(
        &self,
        call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
    ) -> Result<Echo, CanonicalError> {
        let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Echo {
            value: calls.to_string(),
            authorization: None,
            internal_token: None,
            marker: Some(call.endpoint().to_owned()),
        })
    }
}
