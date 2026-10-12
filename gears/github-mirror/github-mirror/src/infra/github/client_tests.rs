use super::{MAX_BODY_BYTES, graphql_url, read_capped};
use crate::domain::error::DomainError;

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
