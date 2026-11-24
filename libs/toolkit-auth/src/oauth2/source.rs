use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OnceCell;

use base64::{Engine as _, engine::general_purpose};
use http::header::AUTHORIZATION;
use url::Url;
use zeroize::Zeroizing;

use super::config::{OAuthClientConfig, TokenEndpoint};
use super::credentials::{ClientCredentials, JWT_BEARER_ASSERTION_TYPE};
use super::error::{HttpOperation, TokenError};
use super::token_watcher::FetchedToken;

/// Token source that exchanges client credentials for an access token using
/// `toolkit-http::HttpClient`.
///
/// Called by [`super::token_watcher::TokenWatcher`] for both the initial fetch
/// and background refreshes.
#[derive(Clone)]
pub struct OAuthTokenSource {
    client: toolkit_http::HttpClient,
    endpoint: TokenEndpoint,
    /// Token endpoint discovered from [`TokenEndpoint::Issuer`], shared by
    /// clones so discovery runs once.
    discovered: Arc<OnceCell<Url>>,
    client_id: String,
    credentials: ClientCredentials,
    /// Pre-joined scopes (space-separated), or `None` when the scope list is
    /// empty.
    scopes: Option<String>,
    extra_headers: Vec<(String, String)>,
    default_ttl: Duration,
    refresh_offset: Duration,
    min_refresh_period: Duration,
}

impl OAuthTokenSource {
    /// Build a new token source from the given configuration.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::Http`] if the underlying `HttpClient` fails to
    /// build.
    pub fn new(config: &OAuthClientConfig) -> Result<Self, TokenError> {
        let mut http_config = config
            .http_config
            .clone()
            .unwrap_or_else(toolkit_http::HttpClientConfig::token_endpoint);
        // A private_key_jwt assertion is single-use (`jti`) and expires in
        // 60 s; an HTTP-layer retry would resend it unchanged, and the IdP may
        // already have consumed it. The token watcher retries with backoff
        // and every attempt signs a new assertion.
        if matches!(config.credentials, ClientCredentials::PrivateKeyJwt(_)) {
            http_config.retry = None;
        }

        let client = toolkit_http::HttpClientBuilder::with_config(http_config)
            .build()
            .map_err(|e| {
                TokenError::Http(crate::http_error::format_http_error(&e, "OAuth2 token"))
            })?;

        let scopes = if config.scopes.is_empty() {
            None
        } else {
            Some(config.scopes.join(" "))
        };

        Ok(Self {
            client,
            endpoint: config.endpoint.clone(),
            discovered: Arc::new(OnceCell::new()),
            client_id: config.client_id.clone(),
            credentials: config.credentials.clone(),
            scopes,
            extra_headers: config.extra_headers.clone(),
            default_ttl: config.default_ttl,
            refresh_offset: config.refresh_offset,
            min_refresh_period: config.min_refresh_period,
        })
    }
}

impl OAuthTokenSource {
    /// Perform a single token exchange and return a [`FetchedToken`] suitable
    /// for caching by the [`super::token_watcher::TokenWatcher`].
    pub(crate) async fn request_token(&mut self) -> Result<FetchedToken, TokenError> {
        let endpoint = match &self.endpoint {
            TokenEndpoint::Direct(url) => url,
            TokenEndpoint::Issuer(issuer) => {
                self.discovered
                    .get_or_try_init(|| {
                        super::discovery::discover_token_endpoint(&self.client, issuer)
                    })
                    .await?
            }
        };
        // -- build form fields ---------------------------------------------------
        let mut fields: Vec<(&str, &str)> = vec![("grant_type", "client_credentials")];

        if let Some(ref scope) = self.scopes {
            fields.push(("scope", scope));
        }

        // -- client authentication -----------------------------------------------
        // Temporary plaintext copies are wrapped in `Zeroizing` so they are
        // scrubbed on drop.
        let assertion;
        let mut basic_auth = None;
        match &self.credentials {
            // Credentials go into the Authorization header.
            ClientCredentials::SecretBasic(secret) => {
                let credentials = Zeroizing::new(format!("{}:{}", self.client_id, secret.expose()));
                let encoded =
                    Zeroizing::new(general_purpose::STANDARD.encode(credentials.as_bytes()));
                basic_auth = Some(Zeroizing::new(format!("Basic {}", encoded.as_str())));
            }
            // Credentials go into the form body.
            ClientCredentials::SecretPost(secret) => {
                fields.push(("client_id", &self.client_id));
                fields.push(("client_secret", secret.expose()));
            }
            // A fresh single-use assertion per request; `aud` defaults to the
            // endpoint resolved above, so discovery has already happened.
            // `client_id` is optional per RFC 7523 but some IdPs require it.
            ClientCredentials::PrivateKeyJwt(key) => {
                assertion = Zeroizing::new(key.client_assertion(
                    &self.client_id,
                    endpoint,
                    time::OffsetDateTime::now_utc(),
                )?);
                fields.push(("client_id", &self.client_id));
                fields.push(("client_assertion_type", JWT_BEARER_ASSERTION_TYPE));
                fields.push(("client_assertion", &assertion));
            }
        }

        // -- build request -------------------------------------------------------
        let mut builder = self.client.post(endpoint.as_str());
        if let Some(header_value) = &basic_auth {
            builder = builder.header(AUTHORIZATION.as_str(), header_value);
        }

        // Apply vendor-specific extra headers.
        for (name, value) in &self.extra_headers {
            builder = builder.header(name, value);
        }

        // A discovered endpoint the client cannot use (bad URL, forbidden
        // scheme) is the identity provider's invalid response, not a
        // configuration error.
        let discovered = matches!(self.endpoint, TokenEndpoint::Issuer(_));
        let send_error = |e: toolkit_http::HttpError| match e {
            toolkit_http::HttpError::InvalidUri { .. }
            | toolkit_http::HttpError::InvalidScheme { .. }
                if discovered =>
            {
                TokenError::InvalidResponse(
                    "token_endpoint in the discovery document cannot be used".into(),
                )
            }
            _ => TokenError::from_http(&e, HttpOperation::TokenRequest, endpoint),
        };
        let response = builder
            .form(fields.as_slice())
            .map_err(send_error)?
            .send()
            .await
            .map_err(send_error)?;

        // -- check status, then parse response ------------------------------------
        // A body that is not a token response is an invalid response, as in
        // discovery; a failure to read it stays an HTTP error.
        let token_resp: super::types::TokenResponse =
            response
                .error_for_status()
                .map_err(|e| TokenError::from_http(&e, HttpOperation::TokenRequest, endpoint))?
                .json()
                .await
                .map_err(|e| match e {
                    toolkit_http::HttpError::Json(_)
                    | toolkit_http::HttpError::BodyTooLarge { .. } => TokenError::InvalidResponse(
                        crate::http_error::format_http_error(&e, "OAuth2 token"),
                    ),
                    _ => TokenError::from_http(&e, HttpOperation::TokenRequest, endpoint),
                })?;

        // -- validate token_type -------------------------------------------------
        if let Some(ref tt) = token_resp.token_type
            && !tt.eq_ignore_ascii_case("bearer")
        {
            return Err(TokenError::UnsupportedTokenType(tt.clone()));
        }

        // -- compute lifetime ----------------------------------------------------
        let lifetime_secs = token_resp.expires_in.unwrap_or(self.default_ttl.as_secs());

        // Compute per-token refresh parameters so that the stale time
        // never exceeds the expiry time, even for short-lived tokens.
        let (freshness_ratio, _min_stale_secs) = refresh_params(
            lifetime_secs,
            &self.refresh_offset,
            &self.min_refresh_period,
        );

        Ok(FetchedToken {
            access_token: token_resp.access_token,
            lifetime_secs,
            freshness_ratio,
        })
    }
}

/// Compute refresh parameters for [`CachedToken`](super::token_watcher::CachedToken).
///
/// Returns `(freshness_ratio, min_stale_secs)` where:
/// - `freshness_ratio` (0.0–1.0): fraction of the token's lifetime during
///   which it is considered **fresh**. After this fraction elapses the token
///   transitions to **stale** and the background watcher attempts a refresh.
/// - `min_stale_secs`: lower bound on the staleness window (the time
///   between the fresh→stale transition and expiry).
///
/// `min_refresh_period` sets a **lower bound on the staleness window**
/// (the minimum time the token spends in the "stale" state before expiry),
/// not a refresh deadline. It is capped to `desired_delay` so it can never
/// push the stale boundary past expiry.
///
/// - Normal case (`offset < lifetime`): stale `offset` seconds before
///   expiry.
/// - Short-lived token (`offset >= lifetime`): stale at 50% of lifetime.
/// - Zero lifetime: immediately stale.
#[allow(clippy::integer_division, clippy::cast_precision_loss)]
fn refresh_params(
    lifetime_secs: u64,
    refresh_offset: &Duration,
    min_refresh_period: &Duration,
) -> (f64, u64) {
    if lifetime_secs == 0 {
        return (0.0, 0);
    }

    let offset = refresh_offset.as_secs();
    let desired_delay = if offset < lifetime_secs {
        lifetime_secs - offset
    } else {
        // Fallback: stale at 50% of lifetime (truncation is fine).
        lifetime_secs / 2
    };

    // Precision loss is negligible — token lifetimes are practical
    // values well within f64 mantissa range.
    let freshness = (desired_delay as f64) / (lifetime_secs as f64);
    let min_stale = min_refresh_period.as_secs().min(desired_delay);

    (freshness, min_stale)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use toolkit_utils::SecretString;

    /// Build a minimal valid config pointing at the mock server.
    fn test_config(server: &MockServer) -> OAuthClientConfig {
        OAuthClientConfig {
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
            ..OAuthClientConfig::new(
                TokenEndpoint::Direct(
                    Url::parse(&format!("http://localhost:{}/token", server.port())).unwrap(),
                ),
                "test-client",
                ClientCredentials::SecretBasic(SecretString::new("test-secret")),
            )
        }
    }

    #[tokio::test]
    async fn request_token_basic_auth_success() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .header_exists("authorization")
                .body_includes("grant_type=client_credentials");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok-123","expires_in":3600,"token_type":"Bearer"}"#);
        });

        let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
        let token = source.request_token().await.unwrap();

        assert_eq!(token.access_token.expose(), "tok-123");
        assert_eq!(token.lifetime_secs, 3600);
        mock.assert();
    }

    #[tokio::test]
    async fn missing_expires_in_uses_default_ttl() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok-456"}"#);
        });

        let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
        let token = source.request_token().await.unwrap();

        // default_ttl from OAuthClientConfig::new() is 5 min = 300s
        assert_eq!(token.lifetime_secs, 300);
        mock.assert();
    }

    #[tokio::test]
    async fn expires_in_zero_honoured() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok-zero","expires_in":0}"#);
        });

        let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
        let token = source.request_token().await.unwrap();

        // Server-provided expires_in is honoured as-is; token watcher handles refresh scheduling.
        assert_eq!(token.lifetime_secs, 0);
        mock.assert();
    }

    #[tokio::test]
    async fn unsupported_token_type_returns_error() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok","token_type":"mac"}"#);
        });

        let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
        let err = source.request_token().await.unwrap_err();

        assert!(
            matches!(err, TokenError::UnsupportedTokenType(ref t) if t == "mac"),
            "expected UnsupportedTokenType(\"mac\"), got: {err}"
        );
        mock.assert();
    }

    #[tokio::test]
    async fn empty_scopes_omits_scope_param() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .body_includes("grant_type=client_credentials")
                .body_excludes("scope");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok"}"#);
        });

        let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
        source.request_token().await.unwrap();
        mock.assert();
    }

    #[tokio::test]
    async fn scopes_are_space_joined() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .body_includes("scope=read+write");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok"}"#);
        });

        let mut cfg = test_config(&server);
        cfg.scopes = vec!["read".into(), "write".into()];
        let mut source = OAuthTokenSource::new(&cfg).unwrap();
        source.request_token().await.unwrap();
        mock.assert();
    }

    #[tokio::test]
    async fn basic_auth_header_present() {
        let server = MockServer::start();

        let expected = format!(
            "Basic {}",
            general_purpose::STANDARD.encode("test-client:test-secret")
        );

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .header("authorization", &expected);
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok"}"#);
        });

        let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
        source.request_token().await.unwrap();
        mock.assert();
    }

    #[tokio::test]
    async fn form_auth_sends_credentials_in_body() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .body_includes("client_id=test-client")
                .body_includes("client_secret=test-secret")
                .body_includes("grant_type=client_credentials");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok"}"#);
        });

        let mut cfg = test_config(&server);
        cfg.credentials = ClientCredentials::SecretPost(SecretString::new("test-secret"));
        let mut source = OAuthTokenSource::new(&cfg).unwrap();
        source.request_token().await.unwrap();
        mock.assert();
    }

    #[tokio::test]
    async fn form_auth_does_not_send_basic_header() {
        let server = MockServer::start();

        // Mock that REQUIRES a Basic auth header — should NOT be hit.
        let basic_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .header_exists("authorization");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok"}"#);
        });

        // Catch-all mock for the POST.
        let fallback_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok"}"#);
        });

        let mut cfg = test_config(&server);
        cfg.credentials = ClientCredentials::SecretPost(SecretString::new("test-secret"));
        let mut source = OAuthTokenSource::new(&cfg).unwrap();
        source.request_token().await.unwrap();

        assert_eq!(
            basic_mock.calls(),
            0,
            "Form auth must not send Authorization header"
        );
        fallback_mock.assert();
    }

    #[tokio::test]
    async fn extra_headers_are_applied() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .header("x-vendor-key", "vendor-value");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok"}"#);
        });

        let mut cfg = test_config(&server);
        cfg.extra_headers = vec![("x-vendor-key".into(), "vendor-value".into())];
        let mut source = OAuthTokenSource::new(&cfg).unwrap();
        source.request_token().await.unwrap();
        mock.assert();
    }

    /// The source retains typed HTTP status without response body diagnostics.
    #[tokio::test]
    async fn http_error_mapped_via_format_http_error() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(401)
                .header("content-type", "application/json")
                .body(r#"{"error":"invalid_client"}"#);
        });

        let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
        let err = source.request_token().await.unwrap_err();

        assert!(
            matches!(
                err,
                TokenError::HttpStatus { operation: HttpOperation::TokenRequest, .. }
                    if err.status() == Some(http::StatusCode::UNAUTHORIZED)
            ),
            "expected Http error with OAuth2 token prefix and 401, got: {err}"
        );
        mock.assert();
    }

    #[tokio::test]
    async fn bearer_case_insensitive() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok","token_type":"bEaReR"}"#);
        });

        let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
        let token = source.request_token().await.unwrap();

        assert_eq!(token.access_token.expose(), "tok");
        mock.assert();
    }

    // -- private_key_jwt --------------------------------------------------------

    /// Records every token request body the mock sees.
    fn record_bodies(
        bodies: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> impl Fn(&HttpMockRequest) -> bool + Send + Sync + 'static {
        let bodies = std::sync::Arc::clone(bodies);
        move |req: &HttpMockRequest| {
            bodies.lock().unwrap().push(req.body_string());
            true
        }
    }

    fn form_field(body: &str, name: &str) -> Option<String> {
        url::form_urlencoded::parse(body.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    }

    #[tokio::test]
    async fn private_key_jwt_sends_assertion_and_no_secret() {
        use crate::oauth2::credentials::tests::{key_and_verifier, verify};
        let server = MockServer::start();

        // Any Authorization header is a failure.
        let header_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .header_exists("authorization");
            then.status(500);
        });
        let bodies = std::sync::Arc::default();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .body_includes("grant_type=client_credentials")
                .body_includes("client_id=test-client")
                .body_includes(
                    "client_assertion_type=urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer",
                )
                .body_includes("client_assertion=")
                .body_excludes("client_secret")
                .is_true(record_bodies(&bodies));
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok-pkjwt","expires_in":3600}"#);
        });

        let (key, verifier) = key_and_verifier(jsonwebtoken::Algorithm::PS256);
        let mut cfg = test_config(&server);
        cfg.credentials = ClientCredentials::PrivateKeyJwt(key);
        let TokenEndpoint::Direct(endpoint) = cfg.endpoint.clone() else {
            panic!("test_config uses a direct token endpoint");
        };
        let mut source = OAuthTokenSource::new(&cfg).unwrap();
        let token = source.request_token().await.unwrap();

        assert_eq!(token.access_token.expose(), "tok-pkjwt");
        mock.assert();
        assert_eq!(header_mock.calls(), 0, "no Authorization header");
        let body = bodies.lock().unwrap()[0].clone();
        let assertion = form_field(&body, "client_assertion").unwrap();
        verify(
            &assertion,
            &verifier,
            jsonwebtoken::Algorithm::PS256,
            "test-client",
            endpoint.as_str(),
        );
    }

    #[tokio::test]
    async fn private_key_jwt_audience_is_the_discovered_endpoint_and_jti_is_fresh() {
        use crate::oauth2::credentials::tests::{key_and_verifier, verify};
        let server = MockServer::start();
        let token_ep = format!("http://localhost:{}/oauth/token", server.port());
        let discovery = server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .header("content-type", "application/json")
                .body(format!(r#"{{"token_endpoint":"{token_ep}"}}"#));
        });
        let bodies = std::sync::Arc::default();
        let _token = server.mock(|when, then| {
            when.method(POST)
                .path("/oauth/token")
                .is_true(record_bodies(&bodies));
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"tok"}"#);
        });

        let (key, verifier) = key_and_verifier(jsonwebtoken::Algorithm::ES256);
        let cfg = OAuthClientConfig {
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
            ..OAuthClientConfig::new(
                TokenEndpoint::Issuer(Url::parse(&server.url("")).unwrap()),
                "test-client",
                ClientCredentials::PrivateKeyJwt(key),
            )
        };
        let mut source = OAuthTokenSource::new(&cfg).unwrap();
        source.request_token().await.unwrap();
        source.request_token().await.unwrap();
        discovery.assert_calls(1);

        let mut jtis = std::collections::BTreeSet::new();
        for body in bodies.lock().unwrap().iter() {
            let assertion = form_field(body, "client_assertion").unwrap();
            // `aud` is the discovered endpoint, not the issuer.
            let (_, claims) = verify(
                &assertion,
                &verifier,
                jsonwebtoken::Algorithm::ES256,
                "test-client",
                &token_ep,
            );
            jtis.insert(claims["jti"].as_str().unwrap().to_owned());
        }
        assert_eq!(jtis.len(), 2, "each request carries a new jti");
    }

    #[tokio::test]
    async fn private_key_jwt_disables_http_retry_so_no_assertion_is_replayed() {
        use crate::oauth2::credentials::tests::key_and_verifier;
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(429);
        });

        // An override that would retry 429 twice for a secret-based client.
        let mut http_config = toolkit_http::HttpClientConfig::for_testing();
        http_config.retry = Some(toolkit_http::RetryConfig {
            max_retries: 2,
            backoff: toolkit_http::ExponentialBackoff::fast(),
            always_retry: std::collections::HashSet::from([
                toolkit_http::RetryTrigger::TOO_MANY_REQUESTS,
            ]),
            ..toolkit_http::RetryConfig::default()
        });
        let mut cfg = test_config(&server);
        cfg.http_config = Some(http_config);

        let mut secret_source = OAuthTokenSource::new(&cfg).unwrap();
        secret_source.request_token().await.unwrap_err();
        mock.assert_calls(3);

        let (key, _) = key_and_verifier(jsonwebtoken::Algorithm::ES256);
        cfg.credentials = ClientCredentials::PrivateKeyJwt(key);
        let mut source = OAuthTokenSource::new(&cfg).unwrap();
        let err = source.request_token().await.unwrap_err();
        assert_eq!(err.status(), Some(http::StatusCode::TOO_MANY_REQUESTS));
        mock.assert_calls(4);
    }

    // -- refresh_params -------------------------------------------------------

    /// Helper: verify that the refresh formula
    /// `max(lifetime * freshness, min_stale) <= lifetime`
    /// holds for the given params.
    #[allow(clippy::cast_precision_loss)]
    fn assert_stale_before_expiry(lifetime: u64, freshness: f64, min_stale: u64) {
        let delay_a = (lifetime as f64) * freshness;
        let delay_b = min_stale as f64;
        let delay = delay_a.max(delay_b);
        assert!(
            delay <= lifetime as f64,
            "stale ({delay}) must not exceed lifetime ({lifetime})"
        );
    }

    #[test]
    fn refresh_normal_token() {
        // 1-hour token, 30-min offset → stale at 50%
        let (r, ms) = refresh_params(3600, &Duration::from_mins(30), &Duration::from_secs(10));
        assert!((r - 0.5).abs() < f64::EPSILON);
        assert_eq!(ms, 10);
        assert_stale_before_expiry(3600, r, ms);
    }

    #[test]
    fn refresh_short_lived_token() {
        // 20-min token, 30-min offset → fallback 0.5
        let (r, ms) = refresh_params(1200, &Duration::from_mins(30), &Duration::from_secs(10));
        assert!((r - 0.5).abs() < f64::EPSILON);
        assert_eq!(ms, 10);
        assert_stale_before_expiry(1200, r, ms);
    }

    #[test]
    fn refresh_equal_lifetime_and_offset() {
        // 30-min token, 30-min offset → fallback 0.5
        let (r, ms) = refresh_params(1800, &Duration::from_mins(30), &Duration::from_secs(10));
        assert!((r - 0.5).abs() < f64::EPSILON);
        assert_stale_before_expiry(1800, r, ms);
    }

    #[test]
    fn refresh_zero_lifetime() {
        // Both values must be zero so stale == expiry.
        let (r, ms) = refresh_params(0, &Duration::from_mins(30), &Duration::from_secs(10));
        assert!((r - 0.0).abs() < f64::EPSILON);
        assert_eq!(ms, 0);
    }

    #[test]
    fn refresh_small_offset() {
        // 5-min token, 1-min offset → stale at 80%
        let (r, ms) = refresh_params(300, &Duration::from_mins(1), &Duration::from_secs(10));
        assert!((r - 0.8).abs() < f64::EPSILON);
        assert_eq!(ms, 10);
        assert_stale_before_expiry(300, r, ms);
    }

    #[test]
    fn refresh_zero_offset() {
        // No offset → stale at 100% (only stale when expired)
        let (r, ms) = refresh_params(3600, &Duration::from_secs(0), &Duration::from_secs(10));
        assert!((r - 1.0).abs() < f64::EPSILON);
        assert_eq!(ms, 10);
        assert_stale_before_expiry(3600, r, ms);
    }

    #[test]
    fn refresh_min_period_exceeds_lifetime() {
        // min_refresh_period (600s) > lifetime (300s) — must be capped
        let (r, ms) = refresh_params(300, &Duration::from_mins(1), &Duration::from_mins(10));
        // desired_delay = 300 - 60 = 240
        assert!((r - 0.8).abs() < f64::EPSILON);
        // min_stale capped to desired_delay, not 600
        assert_eq!(ms, 240);
        assert_stale_before_expiry(300, r, ms);
    }

    #[test]
    fn refresh_zero_lifetime_nonzero_min_period() {
        // expires_in=0 with min_refresh_period=10 — both must be zero
        let (r, ms) = refresh_params(0, &Duration::from_mins(30), &Duration::from_secs(10));
        assert!((r - 0.0).abs() < f64::EPSILON);
        assert_eq!(ms, 0);
    }
}
