use crate::domain::plugin::credentials::{
    CLIENT_ID_REF_KEY, CLIENT_SECRET_REF_KEY, PRIVATE_KEY_REF_KEY, PrivateKeyJwtConfig, SigningAlg,
    credstore_error_kind, parse_credential_ref,
};
use credstore_sdk::CredStoreClientV1;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::infra::plugin::read_credential;
use crate::infra::ttl_cache::TtlCache;
use toolkit_auth::oauth2::types::SecretString;
use toolkit_auth::oauth2::{
    ClientCredentials, OAuthClientConfig, PrivateKeyJwt, RsaJwsAlg, TokenEndpoint, TokenError,
    fetch_token,
};
use toolkit_security::crypto::CryptoError;
use toolkit_security::crypto::signature::{EcdsaP256KeyPair, RsaKeyPair};
use url::Url;
use zeroize::Zeroizing;

use crate::domain::plugin::{AuthContext, AuthPlugin, PluginError};

/// Safety margin subtracted from the IdP-reported `expires_in` when computing
/// cache TTL.  Prevents serving a token that is about to expire while the
/// upstream request is still in flight.
const TOKEN_EXPIRY_SAFETY_MARGIN: Duration = Duration::from_secs(30);

/// Parsed configuration from `AuthContext::config`.
struct OAuth2PluginConfig {
    endpoint: TokenEndpoint,
    client_id_ref: String,
    credential: CredentialSource,
    scopes: Vec<String>,
}

/// Where the client credential comes from, by the plugin's method.
enum CredentialSource {
    /// A client secret under `client_secret_ref`, sent in a Basic header
    /// (`basic`) or the form body.
    Secret { secret_ref: String, basic: bool },
    /// A signing key under `private_key_ref`, with the assertion options.
    PrivateKey(PrivateKeyJwtConfig),
}

impl OAuth2PluginConfig {
    fn parse(
        config: &HashMap<String, String>,
        auth_method: ClientAuthMethod,
    ) -> Result<Self, PluginError> {
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

        // The `private_key_jwt` keys go through the parser upstream writes
        // run, so a stored config passes here too.
        let credential = match auth_method {
            ClientAuthMethod::PrivateKeyJwt => CredentialSource::PrivateKey(
                PrivateKeyJwtConfig::parse(Some(config))
                    .map_err(|e| PluginError::InvalidConfig(e.detail))?,
            ),
            ClientAuthMethod::Basic | ClientAuthMethod::Form => CredentialSource::Secret {
                secret_ref: config
                    .get(CLIENT_SECRET_REF_KEY)
                    .ok_or_else(|| PluginError::InvalidConfig("missing client_secret_ref".into()))?
                    .clone(),
                basic: auth_method == ClientAuthMethod::Basic,
            },
        };

        let scopes = config
            .get("scopes")
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();

        Ok(Self {
            endpoint,
            client_id_ref,
            credential,
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

/// How the plugin authenticates the client to the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAuthMethod {
    /// `client_secret_basic`: the client secret in an `Authorization: Basic`
    /// header.
    Basic,
    /// `client_secret_post`: the client secret in the form body.
    Form,
    /// `private_key_jwt` (RFC 7523 §2.2): a JWT signed with the client's
    /// private key, sent as `client_assertion`. No shared secret is sent.
    PrivateKeyJwt,
}

impl ClientAuthMethod {
    fn tag(self) -> &'static str {
        match self {
            Self::Form => "form",
            Self::Basic => "basic",
            Self::PrivateKeyJwt => "private_key_jwt",
        }
    }
}

const PKCS8_PEM_BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
const PKCS8_PEM_END: &str = "-----END PRIVATE KEY-----";

/// `pem` with its base64 re-wrapped at 64 columns, the only width the strict
/// RFC 7468 decoder in `toolkit_security` takes. Keys pasted into JSON or a
/// secrets UI often end up on one line, and MIME encoders wrap at 76. Text
/// without the `PRIVATE KEY` armor is returned unchanged for the decoder to
/// refuse.
fn rewrap_pkcs8_pem(pem: &str) -> Zeroizing<String> {
    let trimmed = pem.trim();
    let Some(body) = trimmed
        .strip_prefix(PKCS8_PEM_BEGIN)
        .and_then(|rest| rest.strip_suffix(PKCS8_PEM_END))
    else {
        return Zeroizing::new(pem.to_owned());
    };
    // Sized up front: a reallocation would leave an unerased copy behind.
    let mut out = Zeroizing::new(String::with_capacity(
        trimmed.len() + trimmed.len() / 64 + 4,
    ));
    out.push_str(PKCS8_PEM_BEGIN);
    for (i, c) in body
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .enumerate()
    {
        if i % 64 == 0 {
            out.push('\n');
        }
        out.push(c);
    }
    out.push('\n');
    out.push_str(PKCS8_PEM_END);
    out.push('\n');
    out
}

/// The signing key for `private_key_jwt`, from the PKCS#8 PEM `pem`, at any
/// base64 line width. A key that does not parse, or is not of the type
/// `signing_alg` needs, is a misconfiguration naming
/// `auth.config.private_key_ref`; the message never carries key material.
fn signing_key(config: &PrivateKeyJwtConfig, pem: &str) -> Result<PrivateKeyJwt, PluginError> {
    let alg = config.signing_alg;
    let pem = rewrap_pkcs8_pem(pem);
    let key = match alg {
        SigningAlg::Es256 => EcdsaP256KeyPair::from_pkcs8_pem(&pem).map(PrivateKeyJwt::es256),
        SigningAlg::Rs256 => {
            RsaKeyPair::from_pkcs8_pem(&pem).map(|k| PrivateKeyJwt::rsa(k, RsaJwsAlg::Rs256))
        }
        SigningAlg::Ps256 => {
            RsaKeyPair::from_pkcs8_pem(&pem).map(|k| PrivateKeyJwt::rsa(k, RsaJwsAlg::Ps256))
        }
    }
    .map_err(|e| {
        // The PKCS#8 key type, by its algorithm identifier: an RSA-PSS key
        // (`id-RSASSA-PSS`, `openssl genpkey -algorithm RSA-PSS`) is refused.
        // The public point is not derived from the scalar, so a P-256 key
        // without it (`openssl ec -no_public`) is refused too.
        let kind = match alg {
            SigningAlg::Es256 => {
                "EC P-256 (id-ecPublicKey, prime256v1) private key with the public key embedded"
            }
            SigningAlg::Rs256 | SigningAlg::Ps256 => "RSA (rsaEncryption) private key",
        };
        PluginError::InvalidConfig(match e {
            CryptoError::InvalidKey => format!(
                "auth.config.{PRIVATE_KEY_REF_KEY}: the secret is not an unencrypted PKCS#8 PEM \
                 {kind}, which signing_alg {} needs",
                alg.as_str()
            ),
            // A backend or crypto policy failure; the key may be fine.
            other => format!("auth.config.{PRIVATE_KEY_REF_KEY}: cannot load the {kind}: {other}"),
        })
    })?;
    let key = match &config.kid {
        Some(kid) => key.with_kid(kid.clone()),
        None => key,
    };
    Ok(match &config.audience {
        Some(audience) => key.with_audience(audience.clone()),
        None => key,
    })
}

fn build_cache_key(ctx: &AuthContext, auth_method: ClientAuthMethod) -> String {
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

/// Auth plugin implementing the OAuth2 Client Credentials flow, one instance
/// per client authentication method.
pub struct OAuth2ClientCredAuthPlugin {
    credstore: Arc<dyn CredStoreClientV1>,
    auth_method: ClientAuthMethod,
    http_config: Option<toolkit_http::HttpClientConfig>,
    cache: TtlCache<String, CachedToken>,
    cache_ttl: Duration,
}

impl OAuth2ClientCredAuthPlugin {
    /// Create the plugin with a token cache of the given TTL and capacity.
    #[must_use]
    pub fn new(
        credstore: Arc<dyn CredStoreClientV1>,
        auth_method: ClientAuthMethod,
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
        // The shared reference parser; the error names the config key, never
        // the reference or the store's text.
        let secret_ref = parse_credential_ref(cred_ref).map_err(|e| {
            PluginError::InvalidConfig(format!("auth.config.{key}: {}", credstore_error_kind(&e)))
        })?;
        let secret =
            read_credential(self.credstore.as_ref(), security_context, key, &secret_ref).await?;
        std::str::from_utf8(secret.as_bytes())
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
        let config = OAuth2PluginConfig::parse(&ctx.config, self.auth_method)?;
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
        let credentials = match &config.credential {
            CredentialSource::Secret { secret_ref, basic } => {
                let secret = SecretString::new(
                    self.resolve_secret(&ctx.security_context, CLIENT_SECRET_REF_KEY, secret_ref)
                        .await?,
                );
                if *basic {
                    ClientCredentials::SecretBasic(secret)
                } else {
                    ClientCredentials::SecretPost(secret)
                }
            }
            CredentialSource::PrivateKey(pk) => {
                // The PEM is erased on drop.
                let pem = SecretString::new(
                    self.resolve_secret(
                        &ctx.security_context,
                        PRIVATE_KEY_REF_KEY,
                        &pk.private_key_ref,
                    )
                    .await?,
                );
                ClientCredentials::PrivateKeyJwt(signing_key(pk, pem.expose())?)
            }
        };

        let oauth_config = OAuthClientConfig {
            scopes: config.scopes,
            http_config: self.http_config.clone(),
            ..OAuthClientConfig::new(config.endpoint, client_id_str, credentials)
        };

        let fetched = fetch_token(oauth_config).await.map_err(token_error)?;

        // A token the upstream cannot use is never cached or sent: one
        // already expired, or one a header cannot carry.
        if fetched.expires_in.is_zero() {
            return Err(PluginError::IdpInvalidResponse(
                "identity provider sent a token with a zero lifetime".into(),
            ));
        }
        if !is_b64token(fetched.bearer.expose()) {
            return Err(PluginError::IdpInvalidResponse(
                "identity provider sent an access token that is not a bearer token".into(),
            ));
        }
        let authorization = format!("Bearer {}", fetched.bearer.expose());

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

        ctx.set_headers
            .push(("authorization".into(), authorization));

        Ok(())
    }
}

/// Whether `token` is an RFC 6750 §2.1 `b64token`: one or more of
/// `A-Z a-z 0-9 - . _ ~ + /`, then any number of `=`. Anything else (empty,
/// whitespace, control or non-ASCII bytes) no upstream can take as a bearer
/// credential.
fn is_b64token(token: &str) -> bool {
    let body = token.trim_end_matches('=');
    !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
}

/// The auth plugin error for a failed token fetch, by the stage that
/// failed: the identity provider unreachable, timing out or answering 408,
/// 429 or 5xx is unavailable; 400, 401 or 403 to the token request (RFC 6749
/// §5.2 `invalid_client` and friends) means the client credentials were
/// rejected; any other answer, to discovery or the token request, is an
/// invalid response; a configuration the flow cannot run is a
/// misconfiguration. The messages carry no secret (`TokenError` never
/// formats one) and nothing from the response body: an invalid response is
/// reported by a fixed class, since its text can quote the body.
fn token_error(e: TokenError) -> PluginError {
    use http::StatusCode;
    use toolkit_auth::oauth2::HttpOperation;
    let unavailable = |e: &TokenError| PluginError::IdpUnavailable(e.to_string());
    let invalid = |class: &str| PluginError::IdpInvalidResponse(class.to_owned());
    match e {
        TokenError::Http(_) | TokenError::Unavailable(_) => unavailable(&e),
        TokenError::HttpStatus {
            operation, status, ..
        } => {
            // The operation and the status only: with discovery, the URL
            // came from the identity provider.
            let answer = format!("{operation} answered HTTP {}", status.as_u16());
            match status {
                StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS => {
                    PluginError::IdpUnavailable(answer)
                }
                s if s.is_server_error() => PluginError::IdpUnavailable(answer),
                StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                    if operation == HttpOperation::TokenRequest =>
                {
                    PluginError::AuthFailed(format!(
                        "identity provider rejected the client: {answer}"
                    ))
                }
                _ => PluginError::IdpInvalidResponse(answer),
            }
        }
        TokenError::InvalidResponse(_) => invalid("identity provider sent an invalid response"),
        TokenError::UnsupportedTokenType(_) => {
            invalid("identity provider sent an unsupported token type")
        }
        TokenError::InvalidTokenLifetime(_) => {
            invalid("identity provider sent an invalid token lifetime")
        }
        TokenError::ConfigError(_) | TokenError::ClientAssertion(_) => {
            PluginError::InvalidConfig(format!("token fetch failed: {e}"))
        }
        _ => PluginError::Internal(format!("token fetch failed: {e}")),
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
            ClientAuthMethod::Form,
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
            PluginError::SecretNotFound {
                key: CLIENT_ID_REF_KEY,
                ..
            }
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
            PluginError::SecretNotFound {
                key: CLIENT_SECRET_REF_KEY,
                ..
            }
        ));
        let text = err.to_string();
        assert!(text.contains("auth.config.client_secret_ref"), "{text}");
        assert!(!text.contains("test-client-secret"), "{text}");
    }

    /// A credential store outage is unavailable and names the config key;
    /// neither the reference nor the store's text (which may quote it) is in
    /// the error.
    #[tokio::test]
    async fn credstore_outage_is_unavailable_without_the_ref() {
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
        let PluginError::CredentialStoreUnavailable {
            detail: msg,
            retry_after: None,
        } = &err
        else {
            panic!("expected CredentialStoreUnavailable, got {err:?}");
        };
        assert!(msg.contains("credential store unavailable"), "{msg}");
        assert!(msg.contains("auth.config.client_id_ref"), "{msg}");
        assert!(!msg.contains("test-client-id"), "{msg}");
    }

    /// A secret the tenant cannot read is not found, as a missing one is.
    #[tokio::test]
    async fn an_inaccessible_secret_is_not_found() {
        let server = MockServer::start();
        let plugin = make_plugin(Arc::new(crate::domain::test_support::ErroringCredStore(
            || credstore_sdk::CredStoreError::AccessDenied,
        )));
        let mut ctx = make_auth_ctx(make_config(&server));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(
            matches!(
                err,
                PluginError::SecretNotFound {
                    key: CLIENT_ID_REF_KEY,
                    kind: "access denied"
                }
            ),
            "{err:?}"
        );
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

    /// Token endpoint answers map onto the credential mapping: 408, 429 and
    /// 5xx are unavailable, 400/401/403 reject the client, anything else
    /// (an unexpected status, an unparseable body, a token type other than
    /// Bearer) is an invalid identity provider response.
    #[tokio::test]
    async fn token_endpoint_answers_follow_the_credential_mapping() {
        let cases: [(u16, &str, &str); 9] = [
            (500, "internal server error", "unavailable"),
            (503, "", "unavailable"),
            (429, "", "unavailable"),
            (408, "", "unavailable"),
            (400, r#"{"error":"invalid_client"}"#, "rejected"),
            (401, r#"{"error":"invalid_client"}"#, "rejected"),
            (404, "", "invalid response"),
            (200, "not json", "invalid response"),
            (
                200,
                r#"{"access_token":"t","expires_in":3600,"token_type":"mac"}"#,
                "invalid response",
            ),
        ];
        for (status, body, expected) in cases {
            let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(POST).path("/token");
                then.status(status)
                    .header("content-type", "application/json")
                    .body(body);
            });

            let plugin = make_plugin(credstore);
            let mut ctx = make_auth_ctx(make_config(&server));

            let err = plugin.authenticate(&mut ctx).await.unwrap_err();
            let got = match &err {
                PluginError::IdpUnavailable(_) => "unavailable",
                PluginError::AuthFailed(_) => "rejected",
                PluginError::IdpInvalidResponse(_) => "invalid response",
                other => panic!("{status}: unexpected {other:?}"),
            };
            assert_eq!(got, expected, "{status} {body}: {err:?}");
            let text = err.to_string();
            assert!(!text.contains("my-client-secret"), "{text}");
        }
    }

    /// An invalid response is reported by a fixed class: nothing from the
    /// body (a token type, a JSON value) reaches the error, the log or the
    /// REST detail.
    #[tokio::test]
    async fn invalid_response_errors_quote_nothing_from_the_body() {
        let bodies = [
            r#"{"access_token":"t","expires_in":3600,"token_type":"leak-me"}"#,
            r#"{"access_token":"t","expires_in":"leak-me","token_type":"Bearer"}"#,
            "leak-me",
        ];
        for body in bodies {
            let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(POST).path("/token");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(body);
            });
            let plugin = make_plugin(credstore);
            let mut ctx = make_auth_ctx(make_config(&server));

            let err = plugin.authenticate(&mut ctx).await.unwrap_err();
            assert!(
                matches!(err, PluginError::IdpInvalidResponse(_)),
                "{body}: {err:?}"
            );
            assert!(!err.to_string().contains("leak-me"), "{body}: {err}");
        }
    }

    /// Discovery is an unauthenticated GET: a 400, 401 or 403 there does not
    /// mean the client credentials were rejected. It is an invalid response;
    /// 5xx, 408 and 429 stay unavailable.
    #[tokio::test]
    async fn discovery_answers_keep_their_stage() {
        let cases: [(u16, &str); 5] = [
            (403, "invalid response"),
            (401, "invalid response"),
            (404, "invalid response"),
            (503, "unavailable"),
            (429, "unavailable"),
        ];
        for (status, expected) in cases {
            let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/.well-known/openid-configuration");
                then.status(status);
            });
            let plugin = make_plugin(credstore);
            let mut ctx = make_auth_ctx(HashMap::from([
                (
                    "issuer_url".into(),
                    format!("http://localhost:{}", server.port()),
                ),
                ("client_id_ref".into(), "cred://test-client-id".into()),
                (
                    "client_secret_ref".into(),
                    "cred://test-client-secret".into(),
                ),
            ]));

            let err = plugin.authenticate(&mut ctx).await.unwrap_err();
            let got = match &err {
                PluginError::IdpUnavailable(_) => "unavailable",
                PluginError::IdpInvalidResponse(_) => "invalid response",
                other => panic!("{status}: unexpected {other:?}"),
            };
            assert_eq!(got, expected, "{status}: {err:?}");
        }
    }

    /// A token endpoint from the discovery document that the HTTP client
    /// cannot use is the identity provider's invalid response (502), not a
    /// misconfiguration.
    #[tokio::test]
    async fn an_unusable_discovered_endpoint_is_an_invalid_response() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"token_endpoint":"ftp://idp.example/token"}"#);
        });
        let plugin = make_plugin(credstore);
        let mut ctx = make_auth_ctx(HashMap::from([
            (
                "issuer_url".into(),
                format!("http://localhost:{}", server.port()),
            ),
            ("client_id_ref".into(), "cred://test-client-id".into()),
            (
                "client_secret_ref".into(),
                "cred://test-client-secret".into(),
            ),
        ]));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(matches!(err, PluginError::IdpInvalidResponse(_)), "{err:?}");
        assert!(!err.to_string().contains("idp.example"), "{err}");
    }

    #[test]
    fn b64token_accepts_bearer_tokens_only() {
        for ok in [
            "abc",
            "eyJhbGciOi.eyJzdWIi.c2ln",
            "a-b._~+/9",
            "dG9r==",
            "x=",
        ] {
            assert!(is_b64token(ok), "{ok:?}");
        }
        for bad in ["", "=", "==", " ", "a b", "a\tb", "tök", "a=b", "a\u{7}"] {
            assert!(!is_b64token(bad), "{bad:?}");
        }
    }

    /// A token endpoint whose scheme the HTTP client does not support fails
    /// the same way on every retry: a misconfiguration (500), not a
    /// retryable 503.
    #[tokio::test]
    async fn an_unsupported_endpoint_scheme_is_a_misconfiguration() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        let plugin = make_plugin(credstore);
        let mut ctx = make_auth_ctx(HashMap::from([
            ("token_endpoint".into(), "ftp://localhost/token".into()),
            ("client_id_ref".into(), "cred://test-client-id".into()),
            (
                "client_secret_ref".into(),
                "cred://test-client-secret".into(),
            ),
        ]));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        // Past config parsing: the HTTP client refused the scheme.
        assert!(
            matches!(&err, PluginError::InvalidConfig(msg) if msg.starts_with("token fetch failed")),
            "{err:?}"
        );
    }

    /// A token the upstream cannot use (already expired, or not a valid
    /// header value) is an invalid response: never sent, never cached, so
    /// the next request asks the identity provider again.
    #[tokio::test]
    async fn an_unusable_token_is_neither_sent_nor_cached() {
        let bodies = [
            mock_token_response("tok-expired", 0),
            r#"{"access_token":"tok\u0007bell","expires_in":3600,"token_type":"Bearer"}"#
                .to_owned(),
            mock_token_response("", 3600),
            mock_token_response(" ", 3600),
            mock_token_response("tok en", 3600),
            mock_token_response("tök", 3600),
        ];
        for body in bodies {
            let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(POST).path("/token");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(body.clone());
            });
            let plugin = make_plugin(credstore);
            let sc = test_security_context();
            let config = make_config(&server);

            for _ in 0..2 {
                let mut ctx = make_auth_ctx_with_sc(config.clone(), sc.clone());
                let err = plugin.authenticate(&mut ctx).await.unwrap_err();
                assert!(
                    matches!(err, PluginError::IdpInvalidResponse(_)),
                    "{body}: {err:?}"
                );
                assert!(ctx.header_value("authorization").is_none(), "{body}");
                let text = err.to_string();
                assert!(
                    !text.contains("tok-expired") && !text.contains("bell"),
                    "{body}: {text}"
                );
            }
            mock.assert_calls(2);
        }
    }

    /// An identity provider that does not answer at all is unavailable.
    #[tokio::test]
    async fn an_unreachable_token_endpoint_is_unavailable() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(default_creds()));
        // Bind and drop a listener: nothing accepts on the port.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let plugin = make_plugin(credstore);
        let config = HashMap::from([
            (
                "token_endpoint".into(),
                format!("http://127.0.0.1:{port}/token"),
            ),
            ("client_id_ref".into(), "cred://test-client-id".into()),
            (
                "client_secret_ref".into(),
                "cred://test-client-secret".into(),
            ),
        ]);
        let mut ctx = make_auth_ctx(config);

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(matches!(err, PluginError::IdpUnavailable(_)), "{err:?}");
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
            ClientAuthMethod::Basic,
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

    // -----------------------------------------------------------------------
    // Group 10: private_key_jwt
    // -----------------------------------------------------------------------

    /// Test-only keys, generated for these tests (`openssl genpkey`).
    const P256_PEM: &str = include_str!("testdata/p256-private.pem");
    const RSA_PEM: &str = include_str!("testdata/rsa-private.pem");

    /// A line of the base64 body of `pem`: what a leak of the key would show.
    fn key_body_line(pem: &str) -> &str {
        pem.lines().nth(1).unwrap()
    }

    fn pk_creds(pem: &str) -> Vec<(String, String)> {
        vec![
            ("test-client-id".into(), "my-client-id".into()),
            ("test-signing-key".into(), pem.into()),
        ]
    }

    fn pk_config(server: &MockServer, alg: &str) -> HashMap<String, String> {
        HashMap::from([
            (
                "token_endpoint".into(),
                format!("http://localhost:{}/token", server.port()),
            ),
            ("client_id_ref".into(), "cred://test-client-id".into()),
            ("private_key_ref".into(), "cred://test-signing-key".into()),
            ("signing_alg".into(), alg.into()),
        ])
    }

    fn make_pk_plugin(credstore: Arc<dyn CredStoreClientV1>) -> OAuth2ClientCredAuthPlugin {
        OAuth2ClientCredAuthPlugin::new(
            credstore,
            ClientAuthMethod::PrivateKeyJwt,
            Duration::from_secs(3600),
            100,
        )
        .with_http_config(toolkit_http::HttpClientConfig::for_testing())
    }

    fn form_field(body: &str, name: &str) -> Option<String> {
        url::form_urlencoded::parse(body.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    }

    /// Each algorithm signs with its key type; the token request carries the
    /// assertion and no secret, and the token is injected.
    #[tokio::test]
    async fn private_key_jwt_sends_an_assertion_and_no_secret() {
        for (alg, pem) in [("ES256", P256_PEM), ("RS256", RSA_PEM), ("PS256", RSA_PEM)] {
            let server = MockServer::start();
            let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let recorded = Arc::clone(&bodies);
            let mock = server.mock(|when, then| {
                when.method(POST)
                    .path("/token")
                    .header_missing("authorization")
                    .is_true(move |req: &httpmock::HttpMockRequest| {
                        recorded.lock().unwrap().push(req.body_string());
                        true
                    });
                then.status(200)
                    .header("content-type", "application/json")
                    .body(mock_token_response("tok-pkjwt", 3600));
            });

            let plugin = make_pk_plugin(Arc::new(MockCredStoreClient::with_secrets(pk_creds(pem))));
            let mut ctx = make_auth_ctx(pk_config(&server, alg));
            plugin.authenticate(&mut ctx).await.unwrap();

            mock.assert();
            assert_eq!(
                ctx.header_value("authorization").unwrap(),
                "Bearer tok-pkjwt"
            );
            let body = bodies.lock().unwrap()[0].clone();
            assert_eq!(
                form_field(&body, "client_assertion_type").as_deref(),
                Some("urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
                "{alg}"
            );
            assert!(form_field(&body, "client_secret").is_none(), "{alg}");
            let assertion = form_field(&body, "client_assertion").unwrap();
            let header = jsonwebtoken::decode_header(&assertion).unwrap();
            assert_eq!(format!("{:?}", header.alg), alg);
            assert_eq!(header.kid, None, "no kid unless configured");
        }
    }

    /// A key that does not parse, or is not of the type `signing_alg` needs,
    /// is a misconfiguration naming `private_key_ref`, before any token
    /// request, and the error carries no key material.
    #[tokio::test]
    async fn an_unusable_key_is_a_misconfiguration_without_key_material() {
        for (alg, pem) in [
            ("ES256", RSA_PEM),
            ("RS256", P256_PEM),
            ("PS256", P256_PEM),
            ("ES256", "not a key"),
            // PKCS#8 DER base64 without the PEM armor.
            ("ES256", key_body_line(P256_PEM)),
        ] {
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(POST).path("/token");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(mock_token_response("never", 3600));
            });
            let plugin = make_pk_plugin(Arc::new(MockCredStoreClient::with_secrets(pk_creds(pem))));
            let mut ctx = make_auth_ctx(pk_config(&server, alg));

            let err = plugin.authenticate(&mut ctx).await.unwrap_err();
            let text = err.to_string();
            let debug = format!("{err:?}");
            assert!(
                matches!(err, PluginError::InvalidConfig(_)),
                "{alg}: {err:?}"
            );
            assert!(text.contains("auth.config.private_key_ref"), "{text}");
            assert!(text.contains(alg), "{text}");
            for line in [P256_PEM, RSA_PEM]
                .iter()
                .flat_map(|p| p.lines().skip(1).take(3))
            {
                assert!(
                    !text.contains(line) && !debug.contains(line),
                    "{alg}: the error quotes the key"
                );
            }
            assert_eq!(mock.calls(), 0, "{alg}: no token request");
            assert!(ctx.set_headers.is_empty());
        }
    }

    /// Invalid `private_key_jwt` config is refused before the credential
    /// store or the identity provider is asked.
    #[tokio::test]
    async fn private_key_jwt_config_errors_are_misconfigurations() {
        let server = MockServer::start();
        let plugin = make_pk_plugin(Arc::new(MockCredStoreClient::always_failing()));
        let with = |edit: fn(&mut HashMap<String, String>)| {
            let mut c = pk_config(&server, "ES256");
            edit(&mut c);
            c
        };
        for (config, field) in [
            (
                with(|c| {
                    c.insert("client_secret_ref".into(), "cred://s".into());
                }),
                "auth.config.client_secret_ref",
            ),
            (
                with(|c| {
                    c.remove("private_key_ref");
                }),
                "auth.config.private_key_ref",
            ),
            (
                with(|c| {
                    c.insert("signing_alg".into(), "HS256".into());
                }),
                "auth.config.signing_alg",
            ),
        ] {
            let mut ctx = make_auth_ctx(config);
            match plugin.authenticate(&mut ctx).await {
                Err(PluginError::InvalidConfig(msg)) => assert!(msg.starts_with(field), "{msg}"),
                other => panic!("{field}: {other:?}"),
            }
        }
    }

    /// The `kid` reaches the assertion header, and configs that differ only
    /// in the key reference or the `kid` never share a cached token.
    #[tokio::test]
    async fn kid_is_sent_and_distinct_keys_get_distinct_tokens() {
        let server = MockServer::start();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&bodies);
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .is_true(move |req: &httpmock::HttpMockRequest| {
                    recorded.lock().unwrap().push(req.body_string());
                    true
                });
            then.status(200)
                .header("content-type", "application/json")
                .body(mock_token_response("tok", 3600));
        });
        let mut creds = pk_creds(P256_PEM);
        creds.push(("other-signing-key".into(), P256_PEM.into()));
        let plugin = make_pk_plugin(Arc::new(MockCredStoreClient::with_secrets(creds)));
        let sc = test_security_context();

        let mut kid_a = pk_config(&server, "ES256");
        kid_a.insert("kid".into(), "key-a".into());
        let mut kid_b = kid_a.clone();
        kid_b.insert("kid".into(), "key-b".into());
        let mut other_ref = kid_a.clone();
        other_ref.insert("private_key_ref".into(), "cred://other-signing-key".into());

        for config in [&kid_a, &kid_a, &kid_b, &other_ref] {
            let mut ctx = make_auth_ctx_with_sc(config.clone(), sc.clone());
            plugin.authenticate(&mut ctx).await.unwrap();
        }
        mock.assert_calls(3);
        let kids: Vec<Option<String>> = bodies
            .lock()
            .unwrap()
            .iter()
            .map(|b| {
                jsonwebtoken::decode_header(form_field(b, "client_assertion").unwrap())
                    .unwrap()
                    .kid
            })
            .collect();
        assert_eq!(
            kids,
            [
                Some("key-a".into()),
                Some("key-b".into()),
                Some("key-a".into())
            ]
        );
    }
}

// `signing_key` needs no HTTP server, so these run under `fips` too: the key
// types the plugin accepts load with every crypto backend.
#[cfg(test)]
mod signing_key_tests {
    use super::*;

    /// Test-only keys, generated for these tests (`openssl genpkey`).
    const P256_PEM: &str = include_str!("testdata/p256-private.pem");
    const RSA_PEM: &str = include_str!("testdata/rsa-private.pem");
    /// An RSA key with the `id-RSASSA-PSS` algorithm identifier
    /// (`openssl genpkey -algorithm RSA-PSS`), not `rsaEncryption`.
    const RSA_PSS_PEM: &str = include_str!("testdata/rsa-pss-private.pem");

    fn config(alg: SigningAlg) -> PrivateKeyJwtConfig {
        PrivateKeyJwtConfig {
            private_key_ref: "cred://k".into(),
            signing_alg: alg,
            kid: Some("kid-1".into()),
            audience: Some("https://idp.example.com".into()),
        }
    }

    /// The base64 lines of a PEM: what a leak of the key would show.
    fn key_lines(pem: &str) -> impl Iterator<Item = &str> {
        pem.lines().filter(|l| !l.starts_with("-----"))
    }

    /// A P-256 key without the embedded public key
    /// (`openssl ec -no_public | openssl pkcs8 -topk8 -nocrypt`).
    const P256_NO_PUBLIC_PEM: &str = include_str!("testdata/p256-private-no-public.pem");

    /// `pem` re-encoded with its base64 at `width` columns (`None`: one
    /// line), lines ending in `eol`.
    fn rewrapped(pem: &str, width: Option<usize>, eol: &str) -> String {
        let b64: String = key_lines(pem).map(str::trim).collect();
        let lines: Vec<&str> = match width {
            Some(w) => b64
                .as_bytes()
                .chunks(w)
                .map(|c| std::str::from_utf8(c).unwrap())
                .collect(),
            None => vec![b64.as_str()],
        };
        let mut out = String::from(PKCS8_PEM_BEGIN);
        for line in lines {
            out.push_str(eol);
            out.push_str(line);
        }
        out.push_str(eol);
        out.push_str(PKCS8_PEM_END);
        out
    }

    /// A PKCS#8 PEM loads at any base64 line width and with any whitespace
    /// between the armor lines: one line, as pasted into JSON or a secrets UI;
    /// 76 columns, as MIME encoders wrap; CRLF; the whole document on one line.
    #[test]
    fn signing_key_loads_any_base64_line_width() {
        for (alg, pem) in [
            (SigningAlg::Es256, P256_PEM),
            (SigningAlg::Rs256, RSA_PEM),
            (SigningAlg::Ps256, RSA_PEM),
        ] {
            let b64: String = key_lines(pem).map(str::trim).collect();
            for (form, variant) in [
                ("one line", rewrapped(pem, None, "\n")),
                ("76 columns", rewrapped(pem, Some(76), "\n")),
                ("40 columns, CRLF", rewrapped(pem, Some(40), "\r\n")),
                (
                    "document on one line",
                    format!("{PKCS8_PEM_BEGIN} {b64} {PKCS8_PEM_END}"),
                ),
                (
                    "no armor line breaks",
                    format!("{PKCS8_PEM_BEGIN}{b64}{PKCS8_PEM_END}"),
                ),
            ] {
                if let Err(e) = signing_key(&config(alg), &variant) {
                    panic!("{alg:?}, {form}: {e:?}");
                }
            }
        }
    }

    /// Re-wrapping gives the 64-column document `openssl` writes, and leaves
    /// text without the `PRIVATE KEY` armor as it is.
    #[test]
    fn rewrap_pkcs8_pem_writes_64_columns() {
        for pem in [P256_PEM, RSA_PEM] {
            let one_line = rewrapped(pem, None, "\n");
            assert_eq!(
                rewrap_pkcs8_pem(&one_line).as_str(),
                pem.replace("\r\n", "\n")
            );
        }
        for text in [
            "not a key",
            "",
            "-----BEGIN EC PRIVATE KEY-----\nAA==\n-----END EC PRIVATE KEY-----",
        ] {
            assert_eq!(rewrap_pkcs8_pem(text).as_str(), text);
        }
    }

    /// A P-256 key without the embedded public key is refused, at any line
    /// width, with a message saying the public key must be embedded.
    #[test]
    fn a_p256_key_without_the_public_key_is_refused() {
        for pem in [
            P256_NO_PUBLIC_PEM.to_owned(),
            rewrapped(P256_NO_PUBLIC_PEM, None, "\n"),
        ] {
            let Err(PluginError::InvalidConfig(msg)) =
                signing_key(&config(SigningAlg::Es256), &pem)
            else {
                panic!("expected InvalidConfig");
            };
            assert!(
                msg.starts_with("auth.config.private_key_ref:")
                    && msg.contains("with the public key embedded"),
                "{msg}"
            );
            for line in key_lines(P256_NO_PUBLIC_PEM) {
                assert!(!msg.contains(line.trim()), "the message quotes the key");
            }
        }
    }

    /// `Debug` of the credentials built from the key shows the options and
    /// no key material.
    #[test]
    fn signing_key_debug_has_no_key_material() {
        for (alg, pem) in [
            (SigningAlg::Es256, P256_PEM),
            (SigningAlg::Rs256, RSA_PEM),
            (SigningAlg::Ps256, RSA_PEM),
        ] {
            let credentials =
                ClientCredentials::PrivateKeyJwt(signing_key(&config(alg), pem).unwrap());
            let debug = format!("{credentials:?}");
            assert!(debug.contains("kid-1"), "{debug}");
            assert!(debug.contains("https://idp.example.com"), "{debug}");
            for line in key_lines(pem) {
                assert!(!debug.contains(line), "{alg:?}: {debug}");
            }
        }
    }

    /// A key of another type than `signing_alg` needs, an RSA-PSS key, or a
    /// secret that is no PKCS#8 PEM is a misconfiguration naming
    /// `private_key_ref`, the algorithm and the key type it needs, without
    /// key material.
    #[test]
    fn an_unusable_key_names_the_key_type_it_needs() {
        let ec = "EC P-256";
        let rsa = "RSA (rsaEncryption)";
        for (alg, pem, needs) in [
            (SigningAlg::Es256, RSA_PEM, ec),
            (SigningAlg::Rs256, P256_PEM, rsa),
            (SigningAlg::Ps256, P256_PEM, rsa),
            (SigningAlg::Rs256, RSA_PSS_PEM, rsa),
            (SigningAlg::Ps256, RSA_PSS_PEM, rsa),
            (SigningAlg::Es256, RSA_PSS_PEM, ec),
            (SigningAlg::Es256, "not a key", ec),
            (SigningAlg::Rs256, "", rsa),
        ] {
            let Err(PluginError::InvalidConfig(msg)) = signing_key(&config(alg), pem) else {
                panic!("{alg:?}: expected InvalidConfig");
            };
            assert!(
                msg.starts_with("auth.config.private_key_ref:")
                    && msg.contains(alg.as_str())
                    && msg.contains(needs),
                "{alg:?}: {msg}"
            );
            for line in [P256_PEM, RSA_PEM, RSA_PSS_PEM]
                .into_iter()
                .flat_map(key_lines)
            {
                assert!(!msg.contains(line), "{alg:?}: the message quotes the key");
            }
        }
    }
}
