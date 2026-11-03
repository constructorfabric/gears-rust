use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::{GithubClient, MAX_BODY_BYTES, RateLimitHeaders, graphql_url, read_capped};
use crate::domain::error::DomainError;
use crate::domain::sync::SessionTelemetry;

#[test]
fn github_com_serves_graphql_beside_its_rest_root() {
    assert_eq!(
        graphql_url("https://api.github.com"),
        "https://api.github.com/graphql"
    );
    assert_eq!(
        graphql_url("https://api.github.com/"),
        "https://api.github.com/graphql"
    );
}

#[test]
fn an_enterprise_server_serves_graphql_under_api_not_under_v3() {
    assert_eq!(
        graphql_url("https://ghe.local/api/v3"),
        "https://ghe.local/api/graphql"
    );
    assert_eq!(
        graphql_url("https://ghe.local/api/v3/"),
        "https://ghe.local/api/graphql"
    );
}

#[test]
fn any_other_base_keeps_graphql_under_it() {
    assert_eq!(
        graphql_url("http://127.0.0.1:8080"),
        "http://127.0.0.1:8080/graphql"
    );
}

fn response_of(len: usize) -> reqwest::Response {
    reqwest::Response::from(axum::http::Response::new(vec![b'x'; len]))
}

#[tokio::test]
async fn a_body_of_exactly_the_cap_is_read_whole() {
    let cap = usize::try_from(MAX_BODY_BYTES).unwrap();
    let body = read_capped(response_of(cap)).await.unwrap();
    assert_eq!(body.len(), cap);
}

#[tokio::test]
async fn a_body_one_byte_past_the_cap_is_refused() {
    let cap = usize::try_from(MAX_BODY_BYTES).unwrap();
    let error = read_capped(response_of(cap + 1)).await.unwrap_err();
    assert!(
        matches!(&error, DomainError::Internal(message) if message.contains("larger than")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_backoff_set_while_a_request_waits_for_its_permit_holds_that_request_back() {
    let client = GithubClient::new("https://api.github.com".to_owned(), None)
        .unwrap()
        .with_max_concurrent_requests(std::num::NonZeroUsize::MIN);
    let telemetry = SessionTelemetry::default();
    let cancel = CancellationToken::new();
    let held = client.permits.acquire().await.unwrap();

    let admitted = client.admit(&telemetry, &cancel);
    tokio::pin!(admitted);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut admitted)
            .await
            .is_err(),
        "the request must be waiting for the permit, past the controller"
    );

    let limited = RateLimitHeaders {
        retry_after_secs: Some(1),
        ..RateLimitHeaders::default()
    };
    client.controller.observe(&limited, 429, 1).await;
    let released = Instant::now();
    drop(held);

    let admission = admitted.await;

    assert!(admission.is_ok());
    assert!(
        released.elapsed() >= Duration::from_millis(900),
        "the request went out {:?} after the permit came free, inside the backoff",
        released.elapsed()
    );
}
