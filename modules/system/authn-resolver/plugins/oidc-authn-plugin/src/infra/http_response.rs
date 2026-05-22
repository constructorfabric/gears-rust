//! Shared bounded response readers for outbound `IdP` HTTP calls.

use serde::de::DeserializeOwned;

/// Maximum JSON response body accepted from `OIDC`/`OAuth2` endpoints.
///
/// Discovery documents, JWKS sets, and token endpoint responses are expected to
/// be small. Keeping this fixed prevents an `IdP` or intermediary from forcing
/// the plugin to buffer unbounded response bodies.
const HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES: usize = 1024 * 1024;

/// Failure while reading or decoding a bounded HTTP response body.
#[derive(Debug, thiserror::Error)]
pub(super) enum LimitedJsonBodyError {
    /// Response body exceeded the configured response size limit.
    #[error("HTTP response body exceeded limit of {limit} bytes (read {actual} bytes)")]
    BodyTooLarge { limit: usize, actual: usize },
    /// Response body could not be read from the network stream.
    #[error("failed to read HTTP response body: {0}")]
    Read(#[from] reqwest::Error),
    /// Response body was not valid JSON for the requested type.
    #[error("failed to parse HTTP JSON response: {0}")]
    Json(#[from] serde_json::Error),
}

/// Read a successful HTTP response as JSON while enforcing the plugin body cap.
pub(super) async fn read_json_response_limited<T>(
    mut response: reqwest::Response,
) -> Result<T, LimitedJsonBodyError>
where
    T: DeserializeOwned,
{
    let mut body = Vec::new();

    while let Some(chunk) = response.chunk().await? {
        let next_len = body.len().saturating_add(chunk.len());
        if next_len > HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES {
            return Err(LimitedJsonBodyError::BodyTooLarge {
                limit: HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES,
                actual: next_len,
            });
        }
        body.extend_from_slice(&chunk);
    }

    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use httpmock::prelude::{GET, MockServer};
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct TestPayload {
        value: String,
    }

    #[tokio::test]
    async fn read_json_response_limited_parses_json_under_limit() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/json");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"value":"ok"}"#);
        });

        let response = reqwest::Client::new()
            .get(server.url("/json"))
            .send()
            .await
            .expect("mock request should succeed");
        let parsed: TestPayload = read_json_response_limited(response)
            .await
            .expect("bounded JSON body should parse");

        assert_eq!(
            parsed,
            TestPayload {
                value: "ok".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn read_json_response_limited_rejects_body_over_limit() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/too-large");
            then.status(200)
                .header("content-type", "application/json")
                .body("x".repeat(HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES + 1));
        });

        let response = reqwest::Client::new()
            .get(server.url("/too-large"))
            .send()
            .await
            .expect("mock request should succeed");
        let error = read_json_response_limited::<TestPayload>(response)
            .await
            .expect_err("oversized body should be rejected");

        assert!(
            matches!(
                error,
                LimitedJsonBodyError::BodyTooLarge {
                    limit: HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES,
                    actual
                } if actual > HTTP_RESPONSE_BODY_SIZE_LIMIT_BYTES
            ),
            "expected BodyTooLarge, got: {error:?}"
        );
    }
}
