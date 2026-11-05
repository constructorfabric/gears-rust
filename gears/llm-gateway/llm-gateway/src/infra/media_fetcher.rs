//! Media fetcher adapters. No media egress route exists yet (DESIGN §4.10
//! open question), so the only adapter refuses every fetch.

use async_trait::async_trait;
use llm_gateway_sdk::LlmGatewayError;

use crate::domain::ports::{FetchedMedia, MediaFetchLimits, MediaFetcher};

/// [`MediaFetcher`] for a deployment without a media egress route.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRouteMediaFetcher;

#[async_trait]
impl MediaFetcher for NoRouteMediaFetcher {
    fn has_route(&self) -> bool {
        false
    }

    async fn fetch(
        &self,
        _url: &str,
        _limits: &MediaFetchLimits,
    ) -> Result<FetchedMedia, LlmGatewayError> {
        Err(LlmGatewayError::media_not_found())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "media_fetcher_tests.rs"]
mod media_fetcher_tests;
