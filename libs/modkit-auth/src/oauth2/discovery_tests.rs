use super::*;
use httpmock::prelude::*;

fn build_client(_server: &MockServer) -> modkit_http::HttpClient {
    modkit_http::HttpClientBuilder::with_config(modkit_http::HttpClientConfig::for_testing())
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
            TokenError::Http(ref msg)
                if msg.contains("OIDC discovery")
                    && msg.contains("500")
        ),
        "expected Http error with 500 status, got: {err}"
    );
    mock.assert();
}
