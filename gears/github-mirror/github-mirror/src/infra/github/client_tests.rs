use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::{GithubApi, GithubClient, MAX_BODY_BYTES, RateLimitHeaders, graphql_url, read_capped};
use crate::domain::error::DomainError;
use crate::domain::ports::github::{FetchOptions, ForceMode};
use crate::domain::scope::ScopeConfig;
use crate::domain::sync::SessionTelemetry;
use toolkit_security::AccessScope;

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
    let tenant_id = uuid::Uuid::new_v4();
    let options = FetchOptions {
        tenant_id,
        access_scope: AccessScope::for_tenant(tenant_id),
        scope: ScopeConfig::default(),
        force: ForceMode::None,
        since: None,
        cancel: CancellationToken::new(),
        telemetry: std::sync::Arc::new(SessionTelemetry::default()),
        public_repo: std::sync::Arc::default(),
        token_access: std::sync::Arc::default(),
        max_concurrent_tasks: None,
    };
    let held = client.permits.acquire().await.unwrap();

    let admitted = client.admit(GithubApi::Rest, &options);
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
    client
        .build_or_get_pool(Some(options.tenant_id), &options.access_scope)
        .await
        .expect("the pool exists once admit has run")
        .first()
        .controller
        .observe(&limited, 429, 1)
        .await;
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
