use credstore_sdk::{CredStoreClientV1, SecretRef};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use modkit_auth::oauth2::types::{ClientAuthMethod, SecretString};
use modkit_auth::oauth2::{OAuthClientConfig, fetch_token};
use pingora_memory_cache::MemoryCache;
use url::Url;

use crate::domain::plugin::{AuthContext, AuthPlugin, PluginError};

/// Safety margin subtracted from the IdP-reported `expires_in` when computing
/// cache TTL.  Prevents serving a token that is about to expire while the
/// upstream request is still in flight.
const TOKEN_EXPIRY_SAFETY_MARGIN: Duration = Duration::from_secs(30);

/// Parsed configuration from `AuthContext::config`.
struct OAuth2PluginConfig {
    token_endpoint: Option<Url>,
    issuer_url: Option<Url>,
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

        if token_endpoint.is_some() && issuer_url.is_some() {
            return Err(PluginError::InvalidConfig(
                "token_endpoint and issuer_url are mutually exclusive".into(),
            ));
        }
        if token_endpoint.is_none() && issuer_url.is_none() {
            return Err(PluginError::InvalidConfig(
                "one of token_endpoint or issuer_url must be set".into(),
            ));
        }

        let client_id_ref = config
            .get("client_id_ref")
            .ok_or_else(|| PluginError::InvalidConfig("missing client_id_ref".into()))?
            .clone();

        let client_secret_ref = config
            .get("client_secret_ref")
            .ok_or_else(|| PluginError::InvalidConfig("missing client_secret_ref".into()))?
            .clone();

        let scopes = config
            .get("scopes")
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();

        Ok(Self {
            token_endpoint,
            issuer_url,
            client_id_ref,
            client_secret_ref,
            scopes,
        })
    }
}

/// Cached token entry stored alongside the original cache key so that hash
/// collisions inside `TinyUfo` (which hashes keys to `u64` without `Eq`
/// verification) cannot silently return another tenant's token.
#[derive(Clone)]
struct CachedToken {
    key: String,
    token: SecretString,
}

fn build_cache_key(ctx: &AuthContext, auth_method: ClientAuthMethod) -> String {
    format!(
        "{}:{}:{}:{}",
        ctx.security_context.subject_tenant_id(),
        ctx.security_context.subject_id(),
        auth_method_tag(auth_method),
        hash_config(&ctx.config),
    )
}

fn auth_method_tag(method: ClientAuthMethod) -> &'static str {
    match method {
        ClientAuthMethod::Form => "form",
        ClientAuthMethod::Basic => "basic",
    }
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
    auth_method: ClientAuthMethod,
    http_config: Option<modkit_http::HttpClientConfig>,
    cache: MemoryCache<String, CachedToken>,
    cache_ttl: Duration,
}

impl OAuth2ClientCredAuthPlugin {
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
            cache: MemoryCache::new(cache_capacity),
            cache_ttl,
        }
    }

    /// Override the HTTP client config used for token requests.
    #[must_use]
    pub(crate) fn with_http_config(mut self, config: modkit_http::HttpClientConfig) -> Self {
        self.http_config = Some(config);
        self
    }

    /// Resolve a `cred://` reference to its plaintext UTF-8 value.
    async fn resolve_secret(
        &self,
        security_context: &modkit_security::SecurityContext,
        cred_ref: &str,
    ) -> Result<String, PluginError> {
        let raw = cred_ref.strip_prefix("cred://").unwrap_or(cred_ref);
        let secret_ref = SecretRef::new(raw)
            .map_err(|e| PluginError::Internal(format!("invalid secret ref '{raw}': {e}")))?;
        let response = self
            .credstore
            .get(security_context, &secret_ref)
            .await
            .map_err(|e| PluginError::Internal(format!("credstore error: {e}")))?
            .ok_or_else(|| PluginError::SecretNotFound(cred_ref.to_owned()))?;
        std::str::from_utf8(response.value.as_bytes())
            .map(str::to_owned)
            .map_err(|_| PluginError::Internal(format!("secret '{cred_ref}' is not valid UTF-8")))
    }
}

#[async_trait::async_trait]
impl AuthPlugin for OAuth2ClientCredAuthPlugin {
    async fn authenticate(&self, ctx: &mut AuthContext) -> Result<(), PluginError> {
        let config = OAuth2PluginConfig::parse(&ctx.config)?;
        let key = build_cache_key(ctx, self.auth_method);

        // Cache hit — verify key matches to prevent hash-collision leakage.
        let (cached, _status) = self.cache.get(&key);
        if let Some(entry) = cached
            && entry.key == key
        {
            ctx.headers.insert(
                "authorization".into(),
                format!("Bearer {}", entry.token.expose()),
            );
            return Ok(());

            // Hash collision — treat as miss, do not use this entry.
        }

        // Cache miss — resolve credentials and fetch token.
        let client_id_str = self
            .resolve_secret(&ctx.security_context, &config.client_id_ref)
            .await?;
        let client_secret_str = self
            .resolve_secret(&ctx.security_context, &config.client_secret_ref)
            .await?;

        let mut oauth_config = OAuthClientConfig {
            token_endpoint: config.token_endpoint,
            issuer_url: config.issuer_url,
            client_id: client_id_str,
            client_secret: SecretString::new(client_secret_str),
            scopes: config.scopes,
            auth_method: self.auth_method,
            ..Default::default()
        };
        oauth_config.http_config = self.http_config.clone();

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

        // Cache with key for verification — ZeroizeOnDrop fires on eviction.
        self.cache.put(
            &key,
            CachedToken {
                key: key.clone(),
                token: fetched.bearer.clone(),
            },
            Some(ttl),
        );

        ctx.headers.insert(
            "authorization".into(),
            format!("Bearer {}", fetched.bearer.expose()),
        );

        Ok(())
    }
}

#[cfg(test)]
#[path = "oauth2_client_cred_auth_tests.rs"]
mod oauth2_client_cred_auth_tests;
