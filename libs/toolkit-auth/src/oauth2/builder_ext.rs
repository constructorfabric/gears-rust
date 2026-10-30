use http::header::HeaderName;
use tower::ServiceExt;

use super::auto_refresh::{BearerAuthAutoRefreshLayer, BearerAuthAutoRefreshOpts};
use super::layer::BearerAuthLayer;
use super::token::TokenProvider;

/// Extension trait for adding bearer auth to [`toolkit_http::HttpClientBuilder`].
///
/// # Example
///
/// ```ignore
/// use toolkit_auth::HttpClientBuilderExt;
///
/// let token = TokenProvider::new(config)?;
/// let client = HttpClientBuilder::new()
///     .with_bearer_auth(token)
///     .build()?;
/// ```
pub trait HttpClientBuilderExt {
    /// Add `Authorization: Bearer <token>` injection to the HTTP client.
    #[must_use]
    fn with_bearer_auth(self, token: TokenProvider) -> Self;

    /// Add `<header_name>: Bearer <token>` injection to the HTTP client.
    #[must_use]
    fn with_bearer_auth_header(self, token: TokenProvider, header_name: HeaderName) -> Self;

    /// Add bearer auth plus reactive refresh-on-401 to the HTTP client.
    ///
    /// Default options: `Authorization` header, retry on `401` only,
    /// 15-minute throttle on `TokenProvider::invalidate`. See
    /// [`BearerAuthAutoRefreshLayer`] (Rust port of go-appkit's
    /// `AuthBearerRoundTripper`) for the full behavior contract.
    ///
    /// Auto-refresh costs one cheap request clone per call (the original
    /// request is replayed on retry). Prefer
    /// [`HttpClientBuilderExt::with_bearer_auth`] when the upstream is known
    /// not to revoke tokens out-of-band.
    #[must_use]
    fn with_bearer_auth_auto_refresh(self, token: TokenProvider) -> Self;

    /// Add bearer auth plus reactive refresh-on-401 stamped into a custom
    /// header. Other options (predicate, throttle) use defaults.
    #[must_use]
    fn with_bearer_auth_auto_refresh_header(
        self,
        token: TokenProvider,
        header_name: HeaderName,
    ) -> Self;

    /// Add bearer auth plus reactive refresh-on-401 with explicit options.
    /// See [`BearerAuthAutoRefreshOpts`].
    #[must_use]
    fn with_bearer_auth_auto_refresh_opts(
        self,
        token: TokenProvider,
        opts: BearerAuthAutoRefreshOpts,
    ) -> Self;
}

impl HttpClientBuilderExt for toolkit_http::HttpClientBuilder {
    fn with_bearer_auth(self, token: TokenProvider) -> Self {
        let layer = BearerAuthLayer::new(token);
        self.with_auth_layer(move |svc| {
            tower::ServiceBuilder::new()
                .layer(layer)
                .service(svc)
                .boxed_clone()
        })
    }

    fn with_bearer_auth_header(self, token: TokenProvider, header_name: HeaderName) -> Self {
        let layer = BearerAuthLayer::with_header_name(token, header_name);
        self.with_auth_layer(move |svc| {
            tower::ServiceBuilder::new()
                .layer(layer)
                .service(svc)
                .boxed_clone()
        })
    }

    fn with_bearer_auth_auto_refresh(self, token: TokenProvider) -> Self {
        let layer = BearerAuthAutoRefreshLayer::new(token);
        self.with_auth_layer(move |svc| {
            tower::ServiceBuilder::new()
                .layer(layer)
                .service(svc)
                .boxed_clone()
        })
    }

    fn with_bearer_auth_auto_refresh_header(
        self,
        token: TokenProvider,
        header_name: HeaderName,
    ) -> Self {
        self.with_bearer_auth_auto_refresh_opts(
            token,
            BearerAuthAutoRefreshOpts {
                header_name,
                ..BearerAuthAutoRefreshOpts::default()
            },
        )
    }

    fn with_bearer_auth_auto_refresh_opts(
        self,
        token: TokenProvider,
        opts: BearerAuthAutoRefreshOpts,
    ) -> Self {
        let layer = BearerAuthAutoRefreshLayer::with_opts(token, opts);
        self.with_auth_layer(move |svc| {
            tower::ServiceBuilder::new()
                .layer(layer)
                .service(svc)
                .boxed_clone()
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use std::time::Duration;
    use toolkit_utils::SecretString;
    use url::Url;

    use crate::oauth2::config::OAuthClientConfig;

    /// Build a test config pointing at the given mock server for token acquisition.
    fn token_config(server: &MockServer) -> OAuthClientConfig {
        OAuthClientConfig {
            token_endpoint: Some(
                Url::parse(&format!("http://localhost:{}/token", server.port())).unwrap(),
            ),
            client_id: "test-client".into(),
            client_secret: SecretString::new("test-secret"),
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
            jitter_max: Duration::from_millis(0),
            min_refresh_period: Duration::from_millis(100),
            ..Default::default()
        }
    }

    fn token_json(token: &str, expires_in: u64) -> String {
        format!(r#"{{"access_token":"{token}","expires_in":{expires_in},"token_type":"Bearer"}}"#)
    }

    /// The builder obtains a token lazily before sending the first request.
    #[tokio::test]
    async fn with_bearer_auth_injects_header() {
        // OAuth token endpoint
        let oauth_server = MockServer::start();
        let _token_mock = oauth_server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-builder-ext", 3600));
        });

        // Target API server
        let api_server = MockServer::start();
        let api_mock = api_server.mock(|when, then| {
            when.method(GET)
                .path("/api/data")
                .header("authorization", "Bearer tok-builder-ext");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"ok":true}"#);
        });

        let token = TokenProvider::new(token_config(&oauth_server)).unwrap();

        let client = toolkit_http::HttpClientBuilder::new()
            .with_bearer_auth(token)
            .build()
            .unwrap();

        let _resp = client
            .get(&format!("http://localhost:{}/api/data", api_server.port()))
            .send()
            .await
            .unwrap();

        api_mock.assert();
    }

    /// The builder injects credentials using the configured header.
    #[tokio::test]
    async fn with_bearer_auth_header_injects_custom_header() {
        let oauth_server = MockServer::start();
        let _token_mock = oauth_server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-custom-hdr-ext", 3600));
        });

        let api_server = MockServer::start();
        let api_mock = api_server.mock(|when, then| {
            when.method(GET)
                .path("/api/data")
                .header("x-api-key", "Bearer tok-custom-hdr-ext");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"ok":true}"#);
        });

        let token = TokenProvider::new(token_config(&oauth_server)).unwrap();
        token.get().await.unwrap();
        let custom = HeaderName::from_static("x-api-key");

        let client = toolkit_http::HttpClientBuilder::new()
            .with_bearer_auth_header(token, custom)
            .build()
            .unwrap();

        let _resp = client
            .get(&format!("http://localhost:{}/api/data", api_server.port()))
            .send()
            .await
            .unwrap();

        api_mock.assert();
    }

    /// The composed HTTP client replaces a rejected token and replays once.
    #[tokio::test]
    async fn with_bearer_auth_auto_refresh_retries_on_401() {
        // Token endpoint: returns tok-A initially, then tok-B after the
        // 401 triggers an invalidate.
        let oauth_server = MockServer::start();
        let mut initial_mock = oauth_server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-auto-A", 3600));
        });
        let token = TokenProvider::new(token_config(&oauth_server)).unwrap();
        token.get().await.unwrap();
        assert_eq!(initial_mock.calls(), 1, "initial fetch");

        initial_mock.delete();
        let refreshed_mock = oauth_server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-auto-B", 3600));
        });

        // API server: 401 for tok-A, 200 for tok-B.
        let api_server = MockServer::start();
        let unauth_mock = api_server.mock(|when, then| {
            when.method(GET)
                .path("/api/data")
                .header("authorization", "Bearer tok-auto-A");
            then.status(401);
        });
        let ok_mock = api_server.mock(|when, then| {
            when.method(GET)
                .path("/api/data")
                .header("authorization", "Bearer tok-auto-B");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"ok":true}"#);
        });

        let client = toolkit_http::HttpClientBuilder::new()
            .with_bearer_auth_auto_refresh(token)
            .build()
            .unwrap();

        let resp = client
            .get(&format!("http://localhost:{}/api/data", api_server.port()))
            .send()
            .await
            .unwrap();

        assert!(resp.status().is_success(), "retry returned 200");
        unauth_mock.assert_calls(1);
        ok_mock.assert_calls(1);
        assert_eq!(refreshed_mock.calls(), 1, "exactly one invalidate fetch");
    }

    #[tokio::test]
    async fn without_bearer_auth_no_header() {
        let api_server = MockServer::start();

        // Mock that REQUIRES Authorization header — should NOT be hit.
        let auth_mock = api_server.mock(|when, then| {
            when.method(GET)
                .path("/api/data")
                .header_exists("authorization");
            then.status(200).body("authed");
        });

        // Catch-all mock for the GET.
        let fallback_mock = api_server.mock(|when, then| {
            when.method(GET).path("/api/data");
            then.status(200).body("no-auth");
        });

        let client = toolkit_http::HttpClientBuilder::new().build().unwrap();

        let _resp = client
            .get(&format!("http://localhost:{}/api/data", api_server.port()))
            .send()
            .await
            .unwrap();

        assert_eq!(
            auth_mock.calls(),
            0,
            "No Authorization header should be sent without bearer auth"
        );
        fallback_mock.assert();
    }
}
