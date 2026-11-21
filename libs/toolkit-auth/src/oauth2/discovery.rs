use serde::Deserialize;
use url::Url;

use super::error::{HttpOperation, TokenError};

/// Minimal subset of the `OpenID` Connect discovery document.
///
/// Only `token_endpoint` is required; all other fields are silently ignored.
#[derive(Deserialize)]
struct OidcDiscoveryDoc {
    token_endpoint: String,
}

/// Resolve the token endpoint from an OIDC issuer URL.
///
/// Fetches `{issuer_url}/.well-known/openid-configuration` and extracts
/// the `token_endpoint` field. This is a one-time operation at startup.
///
/// # Errors
///
/// Returns [`TokenError::Http`] or [`TokenError::HttpStatus`] if the discovery request fails or returns a
/// non-success status.
/// Returns [`TokenError::InvalidResponse`] if the response body cannot be
/// parsed, the `token_endpoint` field is missing, or it is not a valid URL.
pub async fn discover_token_endpoint(
    client: &toolkit_http::HttpClient,
    issuer_url: &Url,
) -> Result<Url, TokenError> {
    let mut discovery_url = issuer_url.clone();
    let path = format!(
        "{}/.well-known/openid-configuration",
        issuer_url.path().trim_end_matches('/')
    );
    discovery_url.set_path(&path);
    discovery_url.set_query(None);
    discovery_url.set_fragment(None);

    let doc: OidcDiscoveryDoc = client
        .get(discovery_url.as_str())
        .send()
        .await
        .map_err(|e| TokenError::from_http(&e, HttpOperation::Discovery, &discovery_url))?
        .error_for_status()
        .map_err(|e| TokenError::from_http(&e, HttpOperation::Discovery, &discovery_url))?
        .json()
        .await
        // A body that is not a discovery document is an invalid response,
        // as for the token request; a failure to read it stays an HTTP
        // error.
        .map_err(|e| match e {
            toolkit_http::HttpError::Json(_) | toolkit_http::HttpError::BodyTooLarge { .. } => {
                TokenError::InvalidResponse(crate::http_error::format_http_error(
                    &e,
                    "OIDC discovery",
                ))
            }
            _ => TokenError::from_http(&e, HttpOperation::Discovery, &discovery_url),
        })?;

    Url::parse(&doc.token_endpoint).map_err(|e| {
        TokenError::InvalidResponse(format!(
            "invalid token_endpoint URL in discovery document: {e}"
        ))
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn build_client(_server: &MockServer) -> toolkit_http::HttpClient {
        toolkit_http::HttpClientBuilder::with_config(toolkit_http::HttpClientConfig::for_testing())
            .build()
            .unwrap()
    }

    fn issuer_url(server: &MockServer) -> Url {
        Url::parse(&format!("http://localhost:{}", server.port())).unwrap()
    }

    fn issuer_url_trailing_slash(server: &MockServer) -> Url {
        Url::parse(&format!("http://localhost:{}/", server.port())).unwrap()
    }

    #[tokio::test]
    async fn discover_valid_response() {
        let server = MockServer::start();
        let token_ep = format!("http://localhost:{}/oauth/token", server.port());

        let mock = server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .header("content-type", "application/json")
                .body(format!(r#"{{"token_endpoint":"{token_ep}"}}"#));
        });

        let client = build_client(&server);
        let result = discover_token_endpoint(&client, &issuer_url(&server)).await;

        let url = result.unwrap();
        assert_eq!(url.as_str(), token_ep);
        mock.assert();
    }

    #[tokio::test]
    async fn discover_strips_trailing_slash() {
        let server = MockServer::start();
        let token_ep = format!("http://localhost:{}/oauth/token", server.port());

        let mock = server.mock(|when, then| {
            // Must NOT have a double slash — "/.well-known/..." not "//.well-known/..."
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .header("content-type", "application/json")
                .body(format!(r#"{{"token_endpoint":"{token_ep}"}}"#));
        });

        let client = build_client(&server);
        let result = discover_token_endpoint(&client, &issuer_url_trailing_slash(&server)).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
        mock.assert();
    }

    #[tokio::test]
    async fn discover_no_trailing_slash() {
        let server = MockServer::start();
        let token_ep = format!("http://localhost:{}/oauth/token", server.port());

        let mock = server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .header("content-type", "application/json")
                .body(format!(r#"{{"token_endpoint":"{token_ep}"}}"#));
        });

        let client = build_client(&server);
        let result = discover_token_endpoint(&client, &issuer_url(&server)).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
        mock.assert();
    }

    #[tokio::test]
    async fn discover_missing_field() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"authorization_endpoint":"https://example.com/auth"}"#);
        });

        let client = build_client(&server);
        let err = discover_token_endpoint(&client, &issuer_url(&server))
            .await
            .unwrap_err();

        // serde deserialization error for missing required field → InvalidResponse
        assert!(
            matches!(err, TokenError::InvalidResponse(ref msg) if msg.contains("OIDC discovery")),
            "expected InvalidResponse with OIDC discovery prefix, got: {err}"
        );
        mock.assert();
    }

    #[tokio::test]
    async fn discover_invalid_url() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"token_endpoint":"not a valid url"}"#);
        });

        let client = build_client(&server);
        let err = discover_token_endpoint(&client, &issuer_url(&server))
            .await
            .unwrap_err();

        assert!(
            matches!(
                err,
                TokenError::InvalidResponse(ref msg)
                    if msg.contains("invalid token_endpoint")
            ),
            "expected InvalidResponse, got: {err}"
        );
        mock.assert();
    }

    /// Discovery preserves the HTTP rejection as a typed acquisition error.
    #[tokio::test]
    async fn discover_http_error() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(500)
                .header("content-type", "application/json")
                .body(r#"{"error":"server_error"}"#);
        });

        let client = build_client(&server);
        let err = discover_token_endpoint(&client, &issuer_url(&server))
            .await
            .unwrap_err();

        assert!(
            matches!(
                err,
                TokenError::HttpStatus { operation: HttpOperation::Discovery, .. }
                    if err.status() == Some(http::StatusCode::INTERNAL_SERVER_ERROR)
            ),
            "expected Http error with 500 status, got: {err}"
        );
        mock.assert();
    }

    /// A discovery body the connection drops before it ends is a transport
    /// failure (`Http`, so the identity provider is unavailable), not an
    /// invalid document.
    #[tokio::test]
    async fn discover_body_read_failure_is_an_http_error() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                if s.read(&mut buf).await.is_err() {
                    continue;
                }
                s.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{\"tok",
                )
                .await
                .ok();
            }
        });

        let client = toolkit_http::HttpClientBuilder::with_config(
            toolkit_http::HttpClientConfig::for_testing(),
        )
        .build()
        .unwrap();
        let issuer = Url::parse(&format!("http://{addr}")).unwrap();
        let err = discover_token_endpoint(&client, &issuer).await.unwrap_err();
        assert!(matches!(err, TokenError::Http(_)), "{err:?}");
    }
}
