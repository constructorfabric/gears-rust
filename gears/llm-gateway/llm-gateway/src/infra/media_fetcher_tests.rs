//! `NoRouteMediaFetcher`: no media egress route is configured.

use std::time::Duration;

use llm_gateway_sdk::LlmGatewayError;

use super::NoRouteMediaFetcher;
use crate::domain::ports::{MediaFetchLimits, MediaFetcher};

const LIMITS: MediaFetchLimits = MediaFetchLimits {
    timeout: Duration::from_secs(10),
    max_bytes: 1024,
};

#[test]
fn reports_no_route() {
    assert!(!NoRouteMediaFetcher.has_route());
}

#[tokio::test]
async fn every_fetch_is_media_not_found_without_a_reason() {
    let err = NoRouteMediaFetcher
        .fetch("https://example.com/secret-path.png", &LIMITS)
        .await
        .expect_err("no route");
    assert!(
        matches!(err, LlmGatewayError::MediaNotFound { param: None, .. }),
        "{err:?}"
    );
    assert!(!err.to_string().contains("secret-path"));
}
