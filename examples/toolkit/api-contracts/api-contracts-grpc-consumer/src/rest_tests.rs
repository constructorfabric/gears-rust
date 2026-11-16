#![allow(clippy::unwrap_used, clippy::expect_used)]

use api_contracts_sdk::models::{PaymentStatus, PaymentSummary};
use axum::http::header;
use toolkit_canonical_errors::CanonicalError;

use super::{NDJSON, ndjson};

async fn lines(
    items: Vec<Result<PaymentSummary, CanonicalError>>,
) -> (String, Vec<serde_json::Value>) {
    let response = ndjson(Box::pin(futures_util::stream::iter(items)));
    let content_type = response.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .to_owned();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let lines = std::str::from_utf8(&body)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (content_type, lines)
}

fn summary(n: u128) -> PaymentSummary {
    PaymentSummary::new(uuid::Uuid::from_u128(n), 100, "EUR", PaymentStatus::Pending)
}

#[tokio::test]
async fn each_item_is_one_json_line() {
    let (content_type, lines) = lines(vec![Ok(summary(1)), Ok(summary(2))]).await;
    assert_eq!(content_type, NDJSON);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["payment_id"], uuid::Uuid::from_u128(1).to_string());
    assert_eq!(lines[1]["currency"], "EUR");
}

/// The status line is already sent, so an error after the open can only be
/// reported in the body: a last line with the canonical Problem.
#[tokio::test]
async fn an_error_after_the_open_is_a_last_problem_line() {
    let err = CanonicalError::service_unavailable()
        .with_detail("feed lost")
        .create();
    let (_, lines) = lines(vec![Ok(summary(1)), Err(err)]).await;
    assert_eq!(lines.len(), 2);
    assert!(lines[0].get("error").is_none());
    let problem = &lines[1]["error"];
    assert!(
        problem["type"]
            .as_str()
            .unwrap()
            .contains("service_unavailable"),
        "{problem}"
    );
    assert_eq!(problem["status"], 503);
}
