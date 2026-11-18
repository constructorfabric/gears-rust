use crate::domain::plugin::credentials::{
    CLIENT_ID_REF_KEY, CLIENT_SECRET_REF_KEY, credstore_error_kind,
};
use credstore_sdk::{CredStoreClientV1, SecretRef};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::infra::ttl_cache::TtlCache;
use toolkit_auth::oauth2::types::SecretString;
use toolkit_auth::oauth2::{ClientCredentials, OAuthClientConfig, TokenEndpoint, fetch_token};
use url::Url;

use crate::domain::plugin::{AuthContext, AuthPlugin, PluginError};

/// Safety margin subtracted from the IdP-reported `expires_in` when computing
/// cache TTL.  Prevents serving a token that is about to expire while the
/// upstream request is still in flight.
const TOKEN_EXPIRY_SAFETY_MARGIN: Duration = Duration::from_secs(30);

/// Parsed configuration from `AuthContext::config`.
struct OAuth2PluginConfig {
    endpoint: TokenEndpoint,
    client_id_ref: String,
    client_secret_ref: String,
    scopes: Vec<String>,
}

impl OAuth2PluginConfig {
    fn parse(config: &HashMap<String, String>) -> Result<Self, PluginError> {
        let token_endpoint = config
            .get("token_endpoint")
            .map(|s| {
                Url::parse(s).map_err(|e| {
                    PluginError::InvalidConfig(format!("invalid token_endpoint URL: {e}"))
                })
            })
            .transpose()?;

        let issuer_url = config
            .get("issuer_url")
            .map(|s| {
                Url::parse(s)
                    .map_err(|e| PluginError::InvalidConfig(format!("invalid issuer_url URL: {e}")))
            })
            .transpose()?;

        let endpoint = match (token_endpoint, issuer_url) {
            (Some(url), None) => TokenEndpoint::Direct(url),
            (None, Some(issuer)) => TokenEndpoint::Issuer(issuer),
            (Some(_), Some(_)) => {
                return Err(PluginError::InvalidConfig(
                    "token_endpoint and issuer_url are mutually exclusive".into(),
                ));
            }
            (None, None) => {
                return Err(PluginError::InvalidConfig(
                    "one of token_endpoint or issuer_url must be set".into(),
                ));
            }
        };

        let client_id_ref = config
            .get(CLIENT_ID_REF_KEY)
            .ok_or_else(|| PluginError::InvalidConfig("missing client_id_ref".into()))?
            .clone();

        let client_secret_ref = config
            .get(CLIENT_SECRET_REF_KEY)
            .ok_or_else(|| PluginError::InvalidConfig("missing client_secret_ref".into()))?
            .clone();

        let scopes = config
            .get("scopes")
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();

        Ok(Self {
            endpoint,
            client_id_ref,
            client_secret_ref,
            scopes,
        })
    }
}

/// Cached bearer token. The cache compares full keys, so one tenant's entry
/// can never be returned for another's key.
#[derive(Clone)]
struct CachedToken {
    token: SecretString,
}

/// How the plugin presents the resolved client secret to the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretAuthMethod {
    /// `client_secret_basic`: `Authorization: Basic` header.
    Basic,
    /// `client_secret_post`: credentials in the form body.
    Form,
}

impl SecretAuthMethod {
    fn tag(self) -> &'static str {
        match self {
            Self::Form => "form",
            Self::Basic => "basic",
        }
    }

    fn credentials(self, secret: SecretString) -> ClientCredentials {
        match self {
            Self::Basic => ClientCredentials::SecretBasic(secret),
            Self::Form => ClientCredentials::SecretPost(secret),
        }
    }
}

fn build_cache_key(ctx: &AuthContext, auth_method: SecretAuthMethod) -> String {
    format!(
        "{}:{}:{}:{}",
        ctx.security_context.subject_tenant_id(),
        ctx.security_context.subject_id(),
        auth_method.tag(),
        hash_config(&ctx.config),
    )
}

fn hash_config(config: &HashMap<String, String>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut pairs: Vec<_> = config.iter().collect();
    pairs.sort_by_key(|(k, _)| *k);
    for (k, v) in pairs {
        k.hash(&mut hasher);
        v.hash(&mut hasher);
    }
    hasher.finish()
}

/// Auth plugin implementing the OAuth2 Client Credentials flow.
pub struct OAuth2ClientCredAuthPlugin {
    credstore: Arc<dyn CredStoreClientV1>,
    auth_method: SecretAuthMethod,
    http_config: Option<toolkit_http::HttpClientConfig>,
    cache: TtlCache<String, CachedToken>,
    cache_ttl: Duration,
}

impl OAuth2ClientCredAuthPlugin {
    /// Create the plugin with a token cache of the given TTL and capacity.
    #[must_use]
    pub fn new(
        credstore: Arc<dyn CredStoreClientV1>,
        auth_method: SecretAuthMethod,
        cache_ttl: Duration,
        cache_capacity: usize,
    ) -> Self {
        Self {
            credstore,
            auth_method,
            http_config: None,
            cache: TtlCache::new(cache_capacity),
            cache_ttl,
        }
    }

    /// Override the HTTP client config used for token requests.
    #[must_use]
    pub(crate) fn with_http_config(mut self, config: toolkit_http::HttpClientConfig) -> Self {
        self.http_config = Some(config);
        self
    }

    /// Resolve a `cred://` reference to its plaintext UTF-8 value. Errors
    /// name the config `key` the reference came from: the reference and the
    /// store's own text (which may quote it) stay out of errors and logs
    /// (F0008).
    async fn resolve_secret(
        &self,
        security_context: &toolkit_security::SecurityContext,
        key: &'static str,
        cred_ref: &str,
    ) -> Result<String, PluginError> {
        let raw = cred_ref.strip_prefix("cred://").unwrap_or(cred_ref);
        let secret_ref = SecretRef::new(raw).map_err(|e| {
            PluginError::Internal(format!("auth.config.{key}: {}", credstore_error_kind(&e)))
        })?;
        let response = self
            .credstore
            .get(security_context, &secret_ref)
            .await
            .map_err(|e| {
                PluginError::Internal(format!(
                    "credential store error for auth.config.{key}: {}",
                    credstore_error_kind(&e)
                ))
            })?
            .ok_or(PluginError::SecretNotFound(key))?;
        std::str::from_utf8(response.value.as_bytes())
            .map(str::to_owned)
            .map_err(|_| {
                PluginError::Internal(format!(
                    "secret referenced by auth.config.{key} is not valid UTF-8"
                ))
            })
    }
}

#[async_trait::async_trait]
impl AuthPlugin for OAuth2ClientCredAuthPlugin {
    async fn authenticate(&self, ctx: &mut AuthContext) -> Result<(), PluginError> {
        let config = OAuth2PluginConfig::parse(&ctx.config)?;
        let key = build_cache_key(ctx, self.auth_method);

        if let Some(entry) = self.cache.get(&key) {
            ctx.set_headers.push((
                "authorization".into(),
                format!("Bearer {}", entry.token.expose()),
            ));
            return Ok(());
        }

        // Cache miss — resolve credentials and fetch token.
        let client_id_str = self
            .resolve_secret(
                &ctx.security_context,
                CLIENT_ID_REF_KEY,
                &config.client_id_ref,
            )
            .await?;
        let client_secret_str = self
            .resolve_secret(
                &ctx.security_context,
                CLIENT_SECRET_REF_KEY,
                &config.client_secret_ref,
            )
            .await?;

        let oauth_config = OAuthClientConfig {
            scopes: config.scopes,
            http_config: self.http_config.clone(),
            ..OAuthClientConfig::new(
                config.endpoint,
                client_id_str,
                self.auth_method
                    .credentials(SecretString::new(client_secret_str)),
            )
        };

        let fetched = fetch_token(oauth_config)
            .await
            .map_err(|e| PluginError::Internal(format!("token fetch failed: {e}")))?;

        // Use the shorter of config TTL and IdP-reported lifetime (minus safety
        // margin) to avoid serving tokens that are about to expire.
        let ttl = self.cache_ttl.min(
            fetched
                .expires_in
                .saturating_sub(TOKEN_EXPIRY_SAFETY_MARGIN),
        );

        // ZeroizeOnDrop fires on eviction.
        self.cache.put(
            key,
            CachedToken {
                token: fetched.bearer.clone(),
            },
            Some(ttl),
        );

        ctx.set_headers.push((
            "authorization".into(),
            format!("Bearer {}", fetched.bearer.expose()),
        ));

        Ok(())
    }
}

// The two cfg gates are kept separate — `#[cfg(test)]` then
// `#[cfg(not(feature = "fips"))]` — mirroring the same pattern used in
// `libs/toolkit-http/src/client.rs`. The tests below stand up an `httpmock`
// plaintext server and construct token clients via
// `HttpClientConfig::for_testing()`, both of which carry
// `TransportSecurity::AllowInsecureHttp`. Under `--features fips` that also
// needs the FIPS crypto provider and the plaintext opt-in, which this suite
// does not set up, so it is gated out there.
#[cfg(test)]
#[cfg(not(feature = "fips"))]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use httpmock::prelude::*;
    use toolkit_security::SecurityContext;
    use uuid::Uuid;

    use credstore_sdk::CredStoreClientV1;

    use crate::domain::plugin::{AuthContext, AuthPlugin, PluginError};
    use crate::domain::test_support::MockCredStoreClient;

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn mock_token_response(token: &str, expires_in: u64) -> String {
        format!(r#"{{"access_token":"{token}","expires_in":{expires_in},"token_type":"Bearer"}}"#)
    }

    fn make_config(server: &MockServer) -> HashMap<String, String> {
        HashMap::from([
            (
                "token_endpoint".into(),
                format!("http://localhost:{}/token", server.port()),
            ),
            ("client_id_ref".into(), "cred://test-client-id".into()),
            (
                "client_secret_ref".into(),
                "cred://test-client-secret".into(),
            ),
        ])
    }

    fn default_creds() -> Vec<(String, String)> {
        vec![
            ("test-client-id".into(), "my-client-id".into()),
            ("test-client-secret".into(), "my-client-secret".into()),
        ]
    }

    fn test_security_context() -> SecurityContext {
        SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(Uuid::new_v4())
            .build()
            .expect("test security context")
    }

    fn make_auth_ctx(config: HashMap<String, String>) -> AuthContext {
        AuthContext::new(config, test_security_context())
    }

    fn make_plugin(credstore: Arc<dyn CredStoreClientV1>) -> OAuth2ClientCredAuthPlugin {
        OAuth2ClientCredAuthPlugin::new(
            credstore,
            SecretAuthMethod::Form,
            Duration::from_secs(3600),
            100,
        )
        .with_http_config(toolkit_http::HttpClientConfig::for_testing())
    }

    // -----------------------------------------------------------------------
    // Group 1: Config parsing
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn missing_token_endpoint_and_issuer_url_returns_error() {
        let plugin = make_plugin(Arc::new(MockCredStoreClient::empty()));
        let mut ctx = make_auth_ctx(HashMap::from([
            ("client_id_ref".into(), "cred://id".into()),
            ("client_secret_ref".into(), "cred://secret".into()),
        ]));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(
            matches!(err, PluginError::InvalidConfig(ref msg) if msg.contains("one of token_endpoint or issuer_url must be set"))
        );
    }

    #[tokio::test]
    async fn both_token_endpoint_and_issuer_url_returns_error() {
        let plugin = make_plugin(Arc::new(MockCredStoreClient::empty()));
        let mut ctx = make_auth_ctx(HashMap::from([
            (
                "token_endpoint".into(),
                "https://idp.example.com/token".into(),
            ),
            ("issuer_url".into(), "https://idp.example.com".into()),
            ("client_id_ref".into(), "cred://id".into()),
            ("client_secret_ref".into(), "cred://secret".into()),
        ]));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(
            matches!(err, PluginError::InvalidConfig(ref msg) if msg.contains("mutually exclusive"))
        );
    }

    #[tokio::test]
    async fn invalid_token_endpoint_url_returns_error() {
        let plugin = make_plugin(Arc::new(MockCredStoreClient::empty()));
        let mut ctx = make_auth_ctx(HashMap::from([
            ("token_endpoint".into(), "not a url".into()),
            ("client_id_ref".into(), "cred://id".into()),
            ("client_secret_ref".into(), "cred://secret".into()),
        ]));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(
            matches!(err, PluginError::InvalidConfig(ref msg) if msg.contains("invalid token_endpoint URL"))
        );
    }

    #[tokio::test]
    async fn invalid_issuer_url_returns_error() {
        let plugin = make_plugin(Arc::new(MockCredStoreClient::empty()));
        let mut ctx = make_auth_ctx(HashMap::from([
            ("issuer_url".into(), "not a url".into()),
            ("client_id_ref".into(), "cred://id".into()),
            ("client_secret_ref".into(), "cred://secret".into()),
        ]));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(
            matches!(err, PluginError::InvalidConfig(ref msg) if msg.contains("invalid issuer_url URL"))
        );
    }

    #[tokio::test]
    async fn missing_client_id_ref_returns_error() {
        let plugin = make_plugin(Arc::new(MockCredStoreClient::empty()));
        let mut ctx = make_auth_ctx(HashMap::from([
            (
                "token_endpoint".into(),
                "https://idp.example.com/token".into(),
            ),
            ("client_secret_ref".into(), "cred://secret".into()),
        ]));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(
            matches!(err, PluginError::InvalidConfig(ref msg) if msg.contains("missing client_id_ref"))
        );
    }

    #[tokio::test]
    async fn missing_client_secret_ref_returns_error() {
        let plugin = make_plugin(Arc::new(MockCredStoreClient::empty()));
        let mut ctx = make_auth_ctx(HashMap::from([
            (
                "token_endpoint".into(),
                "https://idp.example.com/token".into(),
            ),
            ("client_id_ref".into(), "cred://id".into()),
        ]));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(
            matches!(err, PluginError::InvalidConfig(ref msg) if msg.contains("missing client_secret_ref"))
        );
    }

    // -----------------------------------------------------------------------
    // Group 2: Credential resolution
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn client_id_not_found_returns_secret_not_found() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![(
            "test-client-secret".into(),
            "my-secret".into(),
        )]));
        let server = MockServer::start();
        let plugin = make_plugin(credstore);
        let mut ctx = make_auth_ctx(make_config(&server));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(matches!(
            err,
            PluginError::SecretNotFound(CLIENT_ID_REF_KEY)
        ));
        let text = err.to_string();
        assert!(text.contains("auth.config.client_id_ref"), "{text}");
        assert!(!text.contains("test-client-id"), "{text}");
    }

    #[tokio::test]
    async fn client_secret_not_found_returns_secret_not_found() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![(
            "test-client-id".into(),
            "my-id".into(),
        )]));
        let server = MockServer::start();
        let plugin = make_plugin(credstore);
        let mut ctx = make_auth_ctx(make_config(&server));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(matches!(
            err,
            PluginError::SecretNotFound(CLIENT_SECRET_REF_KEY)
        ));
        let text = err.to_string();
        assert!(text.contains("auth.config.client_secret_ref"), "{text}");
        assert!(!text.contains("test-client-secret"), "{text}");
    }

    /// A credential store failure names the config key; neither the
    /// reference nor the store's text (which may quote it) is in the error.
    #[tokio::test]
    async fn credstore_error_maps_to_internal_without_the_ref() {
        let server = MockServer::start();
        let plugin = make_plugin(Arc::new(crate::domain::test_support::ErroringCredStore(
            || {
                credstore_sdk::CredStoreError::Internal(
                    "store failed reading test-client-id".into(),
                )
            },
        )));
        let mut ctx = make_auth_ctx(make_config(&server));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        let PluginError::Internal(msg) = &err else {
            panic!("expected Internal, got {err:?}");
        };
        assert!(msg.contains("credential store error"), "{msg}");
        assert!(msg.contains("auth.config.client_id_ref"), "{msg}");
        assert!(!msg.contains("test-client-id"), "{msg}");
    }

    #[tokio::test]
    async fn invalid_utf8_secret_returns_internal_error() {
        let server = MockServer::start();
        // Any reference resolves to non-UTF-8 bytes.
        let plugin = make_plugin(Arc::new(MockCredStoreClient::returning_raw_value(vec![
            0xFF, 0xFE,
        ])));
        let mut ctx = make_auth_ctx(make_config(&server));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        let PluginError::Internal(msg) = &err else {
            panic!("expected Internal, got {err:?}");
        };
        assert!(msg.contains("not valid UTF-8"), "{msg}");
        assert!(msg.contains("auth.config.client_id_ref"), "{msg}");
        assert!(!msg.contains("test-client-id"), "{msg}");
    }

    // -----------------------------------------------------------------------
    // Group 3: Token fetch
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn token_fetch_failure_returns_internal_error() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(500).body("internal server error");
        });

        let plugin = make_plugin(credstore);
        let mut ctx = make_auth_ctx(make_config(&server));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(
            matches!(err, PluginError::Internal(ref msg) if msg.contains("token fetch failed"))
        );
    }

    #[tokio::test]
    async fn injects_bearer_token_on_success() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("test-access-token", 3600));
        });

        let plugin = make_plugin(credstore);
        let mut ctx = make_auth_ctx(make_config(&server));

        plugin.authenticate(&mut ctx).await.unwrap();
        assert_eq!(
            ctx.header_value("authorization").unwrap(),
            "Bearer test-access-token"
        );
    }

    // -----------------------------------------------------------------------
    // Group 4: cred:// prefix handling
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn cred_prefix_stripped_for_credstore_lookup() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok-prefixed", 3600));
        });

        let plugin = make_plugin(credstore);
        // Config uses cred:// prefix (default from make_config).
        let mut ctx = make_auth_ctx(make_config(&server));

        plugin.authenticate(&mut ctx).await.unwrap();
        assert_eq!(
            ctx.header_value("authorization").unwrap(),
            "Bearer tok-prefixed"
        );
    }

    #[tokio::test]
    async fn ref_without_cred_prefix_works() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok-bare", 3600));
        });

        let plugin = make_plugin(credstore);
        let mut ctx = make_auth_ctx(HashMap::from([
            (
                "token_endpoint".into(),
                format!("http://localhost:{}/token", server.port()),
            ),
            // No cred:// prefix.
            ("client_id_ref".into(), "test-client-id".into()),
            ("client_secret_ref".into(), "test-client-secret".into()),
        ]));

        plugin.authenticate(&mut ctx).await.unwrap();
        assert_eq!(
            ctx.header_value("authorization").unwrap(),
            "Bearer tok-bare"
        );
    }

    // -----------------------------------------------------------------------
    // Group 5: Scopes & optional fields
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn scopes_forwarded_to_token_endpoint() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .form_urlencoded_tuple_exists("scope");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok-scoped", 3600));
        });

        let plugin = make_plugin(credstore);
        let mut config = make_config(&server);
        config.insert("scopes".into(), "read write".into());
        let mut ctx = make_auth_ctx(config);

        plugin.authenticate(&mut ctx).await.unwrap();
        mock.assert();
    }

    #[tokio::test]
    async fn empty_scopes_when_omitted() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok-no-scope", 3600));
        });

        let plugin = make_plugin(credstore);
        // make_config does not include "scopes".
        let mut ctx = make_auth_ctx(make_config(&server));

        plugin.authenticate(&mut ctx).await.unwrap();
        assert_eq!(
            ctx.header_value("authorization").unwrap(),
            "Bearer tok-no-scope"
        );
    }

    // -----------------------------------------------------------------------
    // Group 6: Auth method variants
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn basic_auth_method_sends_credentials_in_header() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();

        // base64("my-client-id:my-client-secret") = "bXktY2xpZW50LWlkOm15LWNsaWVudC1zZWNyZXQ="
        let mock = server.mock(|when, then| {
            when.method(POST).path("/token").header(
                "authorization",
                "Basic bXktY2xpZW50LWlkOm15LWNsaWVudC1zZWNyZXQ=",
            );
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok-basic", 3600));
        });

        let plugin = OAuth2ClientCredAuthPlugin::new(
            credstore,
            SecretAuthMethod::Basic,
            Duration::from_secs(3600),
            100,
        )
        .with_http_config(toolkit_http::HttpClientConfig::for_testing());
        let mut ctx = make_auth_ctx(make_config(&server));

        plugin.authenticate(&mut ctx).await.unwrap();
        mock.assert();
        assert_eq!(
            ctx.header_value("authorization").unwrap(),
            "Bearer tok-basic"
        );
    }

    #[tokio::test]
    async fn form_auth_method_sends_credentials_in_body_without_basic_header() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();

        let header_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .header_exists("authorization");
            then.status(500);
        });
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .body_includes("client_id=my-client-id")
                .body_includes("client_secret=my-client-secret");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok-form", 3600));
        });

        let plugin = make_plugin(credstore);
        let mut ctx = make_auth_ctx(make_config(&server));

        plugin.authenticate(&mut ctx).await.unwrap();
        mock.assert();
        assert_eq!(header_mock.calls(), 0, "Form must not send a Basic header");
        assert_eq!(
            ctx.header_value("authorization").unwrap(),
            "Bearer tok-form"
        );
    }

    // -----------------------------------------------------------------------
    // Group 7: Token cache behaviour
    // -----------------------------------------------------------------------

    /// Helper that creates an `AuthContext` with an explicit `SecurityContext`.
    fn make_auth_ctx_with_sc(config: HashMap<String, String>, sc: SecurityContext) -> AuthContext {
        AuthContext::new(config, sc)
    }

    #[tokio::test]
    async fn cache_hit_skips_idp_call() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("cached-token", 3600));
        });

        let plugin = make_plugin(credstore);
        let sc = test_security_context();
        let config = make_config(&server);

        // First call — cache miss, hits IdP.
        let mut ctx1 = make_auth_ctx_with_sc(config.clone(), sc.clone());
        plugin.authenticate(&mut ctx1).await.unwrap();
        assert_eq!(
            ctx1.header_value("authorization").unwrap(),
            "Bearer cached-token"
        );

        // Second call — cache hit, no IdP call.
        let mut ctx2 = make_auth_ctx_with_sc(config, sc);
        plugin.authenticate(&mut ctx2).await.unwrap();
        assert_eq!(
            ctx2.header_value("authorization").unwrap(),
            "Bearer cached-token"
        );

        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn different_subject_id_gets_separate_cache_entry() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok-any", 3600));
        });

        let plugin = make_plugin(credstore);
        let tenant_id = Uuid::new_v4();
        let config = make_config(&server);

        let sc_a = SecurityContext::builder()
            .subject_tenant_id(tenant_id)
            .subject_id(Uuid::new_v4())
            .build()
            .unwrap();
        let sc_b = SecurityContext::builder()
            .subject_tenant_id(tenant_id)
            .subject_id(Uuid::new_v4())
            .build()
            .unwrap();

        let mut ctx_a = make_auth_ctx_with_sc(config.clone(), sc_a);
        plugin.authenticate(&mut ctx_a).await.unwrap();

        let mut ctx_b = make_auth_ctx_with_sc(config, sc_b);
        plugin.authenticate(&mut ctx_b).await.unwrap();

        // Two distinct subjects → two IdP calls.
        mock.assert_calls(2);
    }

    #[tokio::test]
    async fn different_tenant_id_gets_separate_cache_entry() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok-any", 3600));
        });

        let plugin = make_plugin(credstore);
        let subject_id = Uuid::new_v4();
        let config = make_config(&server);

        let sc_a = SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(subject_id)
            .build()
            .unwrap();
        let sc_b = SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(subject_id)
            .build()
            .unwrap();

        let mut ctx_a = make_auth_ctx_with_sc(config.clone(), sc_a);
        plugin.authenticate(&mut ctx_a).await.unwrap();

        let mut ctx_b = make_auth_ctx_with_sc(config, sc_b);
        plugin.authenticate(&mut ctx_b).await.unwrap();

        // Two distinct tenants → two IdP calls.
        mock.assert_calls(2);
    }

    #[tokio::test]
    async fn different_config_gets_separate_cache_entry() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok-any", 3600));
        });

        let plugin = make_plugin(credstore);
        let sc = test_security_context();

        // Same SecurityContext, different plugin config (different scopes).
        let mut config_a = make_config(&server);
        config_a.insert("scopes".into(), "read".into());
        let mut config_b = make_config(&server);
        config_b.insert("scopes".into(), "read write".into());

        let mut ctx_a = make_auth_ctx_with_sc(config_a, sc.clone());
        plugin.authenticate(&mut ctx_a).await.unwrap();

        let mut ctx_b = make_auth_ctx_with_sc(config_b, sc);
        plugin.authenticate(&mut ctx_b).await.unwrap();

        mock.assert_calls(2);
    }

    #[tokio::test]
    async fn cache_does_not_leak_token_across_subjects() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();

        // Return different tokens per call using httpmock sequence.
        let mut mock_a = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("token-A", 3600));
        });

        let plugin = make_plugin(credstore);
        let tenant_id = Uuid::new_v4();

        let sc_a = SecurityContext::builder()
            .subject_tenant_id(tenant_id)
            .subject_id(Uuid::new_v4())
            .build()
            .unwrap();

        // Subject A authenticates → gets token-A.
        let config = make_config(&server);
        let mut ctx_a = make_auth_ctx_with_sc(config.clone(), sc_a);
        plugin.authenticate(&mut ctx_a).await.unwrap();
        assert_eq!(
            ctx_a.header_value("authorization").unwrap(),
            "Bearer token-A"
        );

        // Delete the first mock, replace with one that returns token-B.
        mock_a.delete();
        server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("token-B", 3600));
        });

        let sc_b = SecurityContext::builder()
            .subject_tenant_id(tenant_id)
            .subject_id(Uuid::new_v4())
            .build()
            .unwrap();

        // Subject B authenticates → must get token-B, NOT token-A.
        let mut ctx_b = make_auth_ctx_with_sc(config, sc_b);
        plugin.authenticate(&mut ctx_b).await.unwrap();
        assert_eq!(
            ctx_b.header_value("authorization").unwrap(),
            "Bearer token-B"
        );
    }

    #[tokio::test]
    async fn failed_auth_does_not_pollute_cache() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();

        // First mock: fail with 500.
        let mut fail_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(500).body("internal server error");
        });

        let plugin = make_plugin(credstore);
        let sc = test_security_context();
        let config = make_config(&server);

        // First call fails.
        let mut ctx1 = make_auth_ctx_with_sc(config.clone(), sc.clone());
        assert!(plugin.authenticate(&mut ctx1).await.is_err());
        fail_mock.assert_calls(1);

        // Replace with success mock.
        fail_mock.delete();
        let ok_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("retry-token", 3600));
        });

        // Second call with same SecurityContext should retry, not return cached error.
        let mut ctx2 = make_auth_ctx_with_sc(config, sc);
        plugin.authenticate(&mut ctx2).await.unwrap();
        assert_eq!(
            ctx2.header_value("authorization").unwrap(),
            "Bearer retry-token"
        );
        ok_mock.assert_calls(1);
    }

    // -----------------------------------------------------------------------
    // Group 8: expires_in-aware cache TTL
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn short_lived_token_caches_with_reduced_ttl() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                // expires_in=60 → ttl = min(3600, 60-30) = 30s (expires_in wins)
                .body(mock_token_response("tok-short", 60));
        });

        let plugin = make_plugin(credstore);
        let sc = test_security_context();
        let config = make_config(&server);

        // First call — cache miss, fetches token.
        let mut ctx1 = make_auth_ctx_with_sc(config.clone(), sc.clone());
        plugin.authenticate(&mut ctx1).await.unwrap();
        assert_eq!(
            ctx1.header_value("authorization").unwrap(),
            "Bearer tok-short"
        );

        // Second call — cache hit (TTL is 30s, well within test execution).
        let mut ctx2 = make_auth_ctx_with_sc(config, sc);
        plugin.authenticate(&mut ctx2).await.unwrap();
        assert_eq!(
            ctx2.header_value("authorization").unwrap(),
            "Bearer tok-short"
        );

        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn very_short_lived_token_not_cached() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                // expires_in=20 → ttl = min(3600, 20-30) = min(3600, 0) = 0
                // The token cache skips zero-TTL puts → not cached.
                .body(mock_token_response("tok-ephemeral", 20));
        });

        let plugin = make_plugin(credstore);
        let sc = test_security_context();
        let config = make_config(&server);

        // First call — fetches token, but zero TTL means it is not cached.
        let mut ctx1 = make_auth_ctx_with_sc(config.clone(), sc.clone());
        plugin.authenticate(&mut ctx1).await.unwrap();
        assert_eq!(
            ctx1.header_value("authorization").unwrap(),
            "Bearer tok-ephemeral"
        );

        // Second call — cache miss again, must re-fetch.
        let mut ctx2 = make_auth_ctx_with_sc(config, sc);
        plugin.authenticate(&mut ctx2).await.unwrap();

        mock.assert_calls(2);
    }

    #[tokio::test]
    async fn long_lived_token_capped_by_config_ttl() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                // expires_in=86400 → ttl = min(3600, 86370) = 3600 (config wins)
                .body(mock_token_response("tok-long", 86400));
        });

        let plugin = make_plugin(credstore);
        let sc = test_security_context();
        let config = make_config(&server);

        // First call — cache miss, fetches token.
        let mut ctx1 = make_auth_ctx_with_sc(config.clone(), sc.clone());
        plugin.authenticate(&mut ctx1).await.unwrap();
        assert_eq!(
            ctx1.header_value("authorization").unwrap(),
            "Bearer tok-long"
        );

        // Second call — cache hit (config TTL 3600s, well within test execution).
        let mut ctx2 = make_auth_ctx_with_sc(config, sc);
        plugin.authenticate(&mut ctx2).await.unwrap();
        assert_eq!(
            ctx2.header_value("authorization").unwrap(),
            "Bearer tok-long"
        );

        mock.assert_calls(1);
    }

    // -----------------------------------------------------------------------
    // Group 9: Key isolation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn distinct_keys_never_share_a_token() {
        let cache: TtlCache<String, CachedToken> = TtlCache::new(100);
        let key_a = "tenant-a:subject-a:form:12345".to_string();
        cache.put(
            key_a.clone(),
            CachedToken {
                token: SecretString::new("secret-token-a"),
            },
            Some(Duration::from_secs(3600)),
        );

        let hit = cache.get(&key_a).expect("own key hits");
        assert_eq!(hit.token.expose(), "secret-token-a");
        assert!(
            cache
                .get(&"tenant-b:subject-b:form:99999".to_string())
                .is_none(),
            "another tenant's key must miss"
        );
    }
}
