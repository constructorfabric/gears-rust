//! Interactive per-user `OAuth2` authorization-code auth plugin.
//!
//! Unlike [`OAuth2ClientCredAuthPlugin`](super::oauth2_client_cred_auth), this
//! plugin does not mint tokens itself. Per-user tokens are provisioned
//! out-of-band by the OAGW management API (the browser authorization-code
//! flow) and stored in credstore as a JSON [`OAuthTokenRecord`] under a
//! deterministic, per-upstream `SecretRef`. credstore's `Private` sharing
//! mode scopes the record to the calling subject.
//!
//! On each request the plugin:
//! 1. Loads the caller's token record from credstore.
//! 2. Injects the bearer token when it is still valid.
//! 3. Refreshes (and persists the rotated record) when it has expired and a
//!    refresh token is present.
//! 4. Returns [`PluginError::AuthorizationRequired`] when there is no record
//!    or the refresh token was rejected, signalling that the user must
//!    (re-)authorize.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use toolkit_auth::oauth2::types::SecretString;
use uuid::Uuid;

use crate::domain::plugin::{AuthContext, AuthPlugin, PluginError};
use crate::infra::now_unix;
use crate::infra::oauth::OAuthTokenRecord;
use crate::infra::oauth::token_store::UserTokenStore;

/// Default safety margin: refresh once the token is within this window of
/// expiry so it does not lapse mid-request.
const DEFAULT_REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Registry of per-`(subject, upstream_id)` async locks serializing refreshes.
type RefreshLockMap = HashMap<(Uuid, Uuid), Arc<tokio::sync::Mutex<()>>>;

/// Auth plugin implementing the interactive authorization-code flow token
/// store (read + refresh).
pub struct OAuth2AuthCodeAuthPlugin {
    /// Per-user token store; owns the `(subject, upstream_id)` key derivation
    /// shared with the enrollment writer.
    token_store: Arc<dyn UserTokenStore>,
    /// HTTP client config used to build [`Self::http`] on first refresh.
    http_config: toolkit_http::HttpClientConfig,
    /// HTTP client for refresh requests, built once on first use and reused so
    /// the connection pool and TLS/root-cert setup are not rebuilt per request
    /// (`authenticate` runs on the proxy hot path). Lazy so construction needs
    /// no Tokio runtime at wiring time.
    http: tokio::sync::OnceCell<toolkit_http::HttpClient>,
    refresh_margin: Duration,
    /// Per-`(subject, secret_ref)` async locks that serialize token refreshes,
    /// so concurrent requests for the same credential do not all refresh in
    /// parallel (which, with refresh-token rotation, would fail all but one).
    /// The map itself is guarded by a synchronous lock; the per-key mutex is
    /// async because it is held across the refresh network round-trip.
    refresh_locks: parking_lot::Mutex<RefreshLockMap>,
}

impl OAuth2AuthCodeAuthPlugin {
    /// Build the plugin with the default token-endpoint HTTP client config.
    #[must_use]
    pub fn new(token_store: Arc<dyn UserTokenStore>) -> Self {
        Self::with_http_config(
            token_store,
            toolkit_http::HttpClientConfig::token_endpoint(),
        )
    }

    /// Build the plugin with an explicit HTTP client config (test seam).
    #[must_use]
    pub fn with_http_config(
        token_store: Arc<dyn UserTokenStore>,
        http_config: toolkit_http::HttpClientConfig,
    ) -> Self {
        Self {
            token_store,
            http_config,
            http: tokio::sync::OnceCell::new(),
            refresh_margin: DEFAULT_REFRESH_MARGIN,
            refresh_locks: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Return the shared HTTP client, building it once on first call.
    async fn http(&self) -> Result<&toolkit_http::HttpClient, PluginError> {
        self.http
            .get_or_try_init(|| async {
                toolkit_http::HttpClientBuilder::with_config(self.http_config.clone())
                    .build()
                    .map_err(|e| PluginError::Internal(format!("failed to build HTTP client: {e}")))
            })
            .await
    }

    /// Fetch (or create) the async lock serializing refreshes for a given
    /// subject + upstream. The map critical section is synchronous.
    fn refresh_lock(&self, subject: Uuid, upstream_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        let key = (subject, upstream_id);
        self.refresh_locks.lock().entry(key).or_default().clone()
    }

    /// Release interest in a refresh lock and evict its map entry when no other
    /// task holds a reference, keeping [`Self::refresh_locks`] from growing
    /// without bound. Must be called only after the per-key guard is dropped.
    ///
    /// The synchronous map lock makes this race-free: a concurrent caller for
    /// the same key either already cloned this `Arc` (strong count exceeds the
    /// map entry plus our reference, so the entry is retained and serialization
    /// is preserved) or is blocked on this same map lock and cannot clone until
    /// we return. The pointer-equality check ensures we never remove a distinct
    /// entry that was re-created for the key after an earlier eviction.
    fn release_refresh_lock(
        &self,
        subject: Uuid,
        upstream_id: Uuid,
        lock: &Arc<tokio::sync::Mutex<()>>,
    ) {
        let key = (subject, upstream_id);
        let mut map = self.refresh_locks.lock();
        // Strong count of 2 == the map entry + our `lock`: no other task is
        // waiting on or using it.
        if map
            .get(&key)
            .is_some_and(|existing| Arc::ptr_eq(existing, lock))
            && Arc::strong_count(lock) == 2
        {
            map.remove(&key);
        }
    }

    /// Serialized refresh critical section, run while holding the per-key
    /// refresh mutex. Split out so [`authenticate`](Self::authenticate) can
    /// always evict the lock entry after the guard is dropped, on every path.
    async fn refresh_under_lock(
        &self,
        ctx: &mut AuthContext,
        resource: &str,
        margin: i64,
    ) -> Result<(), PluginError> {
        // Re-load and re-check under the lock: a concurrent refresh may have
        // already renewed the record while we waited, in which case we reuse it.
        let current = self
            .load_record(ctx)
            .await?
            .ok_or_else(|| PluginError::AuthorizationRequired(resource.to_owned()))?;
        if current.expires_at_unix > now_unix() + margin {
            inject_bearer(ctx, &current.access_token);
            return Ok(());
        }

        // Still expired: perform the refresh against the freshly-loaded record.
        match self.refresh(&current).await {
            Ok(tokens) => {
                let refreshed = OAuthTokenRecord {
                    version: OAuthTokenRecord::CURRENT_VERSION,
                    client_id: current.client_id.clone(),
                    client_secret: current.client_secret.clone(),
                    token_endpoint: current.token_endpoint.clone(),
                    access_token: tokens.access_token.expose().to_owned(),
                    refresh_token: tokens
                        .refresh_token
                        .as_ref()
                        .map(|s| s.expose().to_owned())
                        .or_else(|| current.refresh_token.clone()),
                    expires_at_unix: now_unix().saturating_add(
                        i64::try_from(tokens.expires_in.as_secs()).unwrap_or(i64::MAX),
                    ),
                    scope: tokens.scope.clone().or_else(|| current.scope.clone()),
                };
                self.persist(ctx, &refreshed).await?;
                inject_bearer(ctx, &refreshed.access_token);
                Ok(())
            }
            Err(PluginError::AuthorizationRequired(_)) => {
                // Refresh rejected or unavailable: drop the stale record so the
                // user re-authorizes. Only delete when the stored record still
                // matches the version we attempted, so a newer record written
                // out-of-band (e.g. a fresh re-authorization) is never clobbered.
                if let Ok(Some(latest)) = self.load_record(ctx).await
                    && latest == current
                    && let Err(e) = self
                        .token_store
                        .delete(&ctx.security_context, ctx.upstream_id)
                        .await
                {
                    // A failed eviction leaves an invalid record that re-fails
                    // every request; surface the underlying token-store problem.
                    tracing::warn!(
                        upstream_id = %ctx.upstream_id,
                        error = %e,
                        "failed to delete stale OAuth token record after rejected refresh"
                    );
                }
                Err(PluginError::AuthorizationRequired(resource.to_owned()))
            }
            Err(other) => Err(other),
        }
    }

    /// The protected-resource identifier from the auth binding config, surfaced
    /// in `AuthorizationRequired` so the caller can prompt re-authorization.
    fn resource_hint(ctx: &AuthContext) -> String {
        ctx.config.get("resource").cloned().unwrap_or_default()
    }

    async fn load_record(
        &self,
        ctx: &AuthContext,
    ) -> Result<Option<OAuthTokenRecord>, PluginError> {
        self.token_store
            .load(&ctx.security_context, ctx.upstream_id)
            .await
            .map_err(|e| PluginError::Internal(e.to_string()))
    }

    async fn persist(
        &self,
        ctx: &AuthContext,
        record: &OAuthTokenRecord,
    ) -> Result<(), PluginError> {
        self.token_store
            .store(&ctx.security_context, ctx.upstream_id, record)
            .await
            .map_err(|e| PluginError::Internal(e.to_string()))
    }

    async fn refresh(
        &self,
        record: &OAuthTokenRecord,
    ) -> Result<toolkit_auth::oauth2::TokenSet, PluginError> {
        let refresh = record.refresh_token.as_ref().ok_or_else(|| {
            PluginError::AuthorizationRequired("no refresh token available".to_owned())
        })?;
        let client_secret = record
            .client_secret
            .as_ref()
            .map(|s| SecretString::new(s.clone()));
        let refresh_ss = SecretString::new(refresh.clone());
        let scopes: Vec<String> = record
            .scope
            .as_deref()
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();

        let http = self.http().await?;

        toolkit_auth::oauth2::refresh_token(
            http,
            &record.token_endpoint,
            &record.client_id,
            client_secret.as_ref(),
            &refresh_ss,
            &scopes,
        )
        .await
        .map_err(|e| match e {
            toolkit_auth::oauth2::TokenError::RefreshRejected(_) => {
                PluginError::AuthorizationRequired("refresh token rejected".to_owned())
            }
            other => PluginError::Internal(format!("token refresh failed: {other}")),
        })
    }
}

fn inject_bearer(ctx: &mut AuthContext, token: &str) {
    ctx.headers
        .insert("authorization".into(), format!("Bearer {token}"));
}

#[async_trait::async_trait]
impl AuthPlugin for OAuth2AuthCodeAuthPlugin {
    async fn authenticate(&self, ctx: &mut AuthContext) -> Result<(), PluginError> {
        let resource = Self::resource_hint(ctx);

        let record = self
            .load_record(ctx)
            .await?
            .ok_or_else(|| PluginError::AuthorizationRequired(resource.clone()))?;

        let margin = i64::try_from(self.refresh_margin.as_secs()).unwrap_or(60);
        if record.expires_at_unix > now_unix() + margin {
            inject_bearer(ctx, &record.access_token);
            return Ok(());
        }

        // Expired (or near-expiry): serialize refreshes per (subject,
        // upstream_id) so concurrent requests do not all refresh at once.
        let subject = ctx.security_context.subject_id();
        let refresh_lock = self.refresh_lock(subject, ctx.upstream_id);
        let result = {
            let _guard = refresh_lock.lock().await;
            self.refresh_under_lock(ctx, &resource, margin).await
        };
        // Guard dropped above; evict the map entry when no other task holds it
        // so the registry does not grow unbounded across subjects/upstreams.
        self.release_refresh_lock(subject, ctx.upstream_id, &refresh_lock);
        result
    }
}

#[cfg(test)]
#[path = "oauth2_auth_code_auth_tests.rs"]
mod oauth2_auth_code_auth_tests;
