//! Interactive OAuth2 authorization-code enrollment service.
//!
//! The flow spans two calls:
//!
//! 1. [`begin`](OAuthEnrollmentServiceImpl::begin) discovers the authorization
//!    server for the upstream's protected resource, dynamically registers a
//!    PKCE client, generates PKCE + CSRF state, captures the initiating subject
//!    in an in-memory [`PendingAuthorization`] keyed by `state`, and returns
//!    the browser authorization URL.
//! 2. [`complete`](OAuthEnrollmentServiceImpl::complete) is driven by the
//!    unauthenticated browser callback: it takes the pending entry by `state`,
//!    exchanges the authorization `code`, and persists the per-user token via
//!    the [`UserTokenStore`] on behalf of the captured subject.
//!
//! [`revoke`](OAuthEnrollmentServiceImpl::revoke) and
//! [`status`](OAuthEnrollmentServiceImpl::status) act on the stored token via
//! the token store. Durable secret material is scoped to the acting subject by
//! the token store (`Private` sharing); transient PKCE/CSRF state lives only in
//! the in-memory pending store and never touches durable storage.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_auth::oauth2::authcode;
use toolkit_auth::oauth2::types::SecretString;
use toolkit_security::SecurityContext;
use url::Url;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::services::{
    ControlPlaneService, OAuthBeginOutcome, OAuthConnectionStatus, OAuthEnrollmentService,
};
use crate::infra::now_unix;
use crate::infra::oauth::OAuthTokenRecord;
use crate::infra::oauth::pending_store::{PendingAuthorization, PendingAuthorizationStore};
use crate::infra::oauth::token_store::UserTokenStore;

/// Config key on the upstream auth binding naming the protected-resource URL
/// used to discover the authorization server. Optional — when absent (or
/// blank), discovery falls back to probing the upstream's own endpoint for an
/// RFC 9728 `WWW-Authenticate` challenge.
const CONFIG_RESOURCE: &str = "resource";

/// Config key on the upstream auth binding carrying the space/comma-separated
/// OAuth scopes to request. Scopes are sourced here (never from the caller) so
/// grants are governed by the upstream's stored configuration.
const CONFIG_SCOPES: &str = "scopes";

/// Split a `scopes` config value on whitespace or commas into a scope list,
/// dropping empty entries.
fn parse_scopes(raw: &str) -> Vec<String> {
    raw.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Build the base URL to probe for an RFC 9728 `WWW-Authenticate` challenge,
/// from the upstream's first configured endpoint. Used only when the auth
/// binding does not name an explicit `resource` URL.
fn probe_url_for_upstream(upstream: &crate::domain::model::Upstream) -> Result<Url, DomainError> {
    use crate::domain::model::Scheme;
    let endpoint = upstream.server.endpoints.first().ok_or_else(|| {
        DomainError::validation("upstream has no endpoints to probe for OAuth metadata")
    })?;
    let scheme = match endpoint.scheme {
        Scheme::Http => "http",
        _ => "https",
    };
    let raw = format!("{scheme}://{}:{}/", endpoint.host, endpoint.port);
    Url::parse(&raw)
        .map_err(|e| DomainError::internal(format!("invalid upstream endpoint URL: {e}")))
}

/// Enrollment service backed by the control plane (upstream lookup), the
/// per-user [`UserTokenStore`], an in-memory [`PendingAuthorizationStore`], and
/// an HTTP client (discovery / dynamic client registration / token exchange).
pub(crate) struct OAuthEnrollmentServiceImpl {
    cp: Arc<dyn ControlPlaneService>,
    token_store: Arc<dyn UserTokenStore>,
    pending: Arc<PendingAuthorizationStore>,
    /// OAGW's own OAuth callback URL, registered as the `redirect_uri`. This is
    /// a deployment value, never caller-supplied; `None` disables the flow.
    oauth_callback_url: Option<String>,
    /// Exact-match allowlist of permitted post-callback `return_to` URLs.
    return_to_allowlist: Vec<String>,
    /// HTTP client config used to build [`Self::http`] on first use.
    http_config: toolkit_http::HttpClientConfig,
    /// HTTP client, built once on first use and reused across operations.
    http: tokio::sync::OnceCell<toolkit_http::HttpClient>,
}

impl OAuthEnrollmentServiceImpl {
    /// Build with an explicit HTTP client config, so callers can propagate the
    /// gear's effective token transport policy (e.g. `allow_http_upstream`),
    /// plus the deployment-controlled `redirect_uri` (`oauth_callback_url`) and
    /// `return_to` allowlist.
    #[must_use]
    pub(crate) fn with_http_config(
        cp: Arc<dyn ControlPlaneService>,
        token_store: Arc<dyn UserTokenStore>,
        pending: Arc<PendingAuthorizationStore>,
        oauth_callback_url: Option<String>,
        return_to_allowlist: Vec<String>,
        http_config: toolkit_http::HttpClientConfig,
    ) -> Self {
        Self {
            cp,
            token_store,
            pending,
            oauth_callback_url,
            return_to_allowlist,
            http_config,
            http: tokio::sync::OnceCell::new(),
        }
    }

    /// Return the shared HTTP client, building it once on first call.
    async fn http(&self) -> Result<&toolkit_http::HttpClient, DomainError> {
        self.http
            .get_or_try_init(|| async {
                toolkit_http::HttpClientBuilder::with_config(self.http_config.clone())
                    .build()
                    .map_err(|e| DomainError::internal(format!("failed to build HTTP client: {e}")))
            })
            .await
    }
}

#[async_trait]
impl OAuthEnrollmentService for OAuthEnrollmentServiceImpl {
    async fn begin(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
        return_to: String,
        client_name: String,
    ) -> Result<OAuthBeginOutcome, DomainError> {
        // `redirect_uri` is the deployment-configured OAGW callback URL, never
        // caller-supplied — an attacker-chosen value is the canonical
        // authorization-code interception vector.
        let redirect_uri = self.oauth_callback_url.clone().ok_or_else(|| {
            DomainError::internal("interactive OAuth is not configured: set `oauth_callback_url`")
        })?;

        // `return_to` is consumer-supplied but must be an exact member of the
        // deployment allowlist before the browser is ever redirected to it.
        if !self.return_to_allowlist.iter().any(|a| a == &return_to) {
            return Err(DomainError::validation(
                "return_to is not in the configured allowlist",
            ));
        }

        let upstream = self.cp.get_upstream(ctx, upstream_id).await?;
        let auth = upstream
            .auth
            .as_ref()
            .ok_or_else(|| DomainError::validation("upstream has no auth configuration"))?;
        let auth_config = auth
            .config
            .as_ref()
            .ok_or_else(|| DomainError::validation("upstream auth binding has no config"))?;

        // Scopes come from the upstream's stored config, never from the caller.
        let scopes = auth_config
            .get(CONFIG_SCOPES)
            .map(|raw| parse_scopes(raw))
            .unwrap_or_default();

        let http = self.http().await?;

        // Resolve authorization-server metadata: prefer an explicit
        // protected-resource URL from config; otherwise discover it from the
        // upstream's own RFC 9728 `WWW-Authenticate` challenge.
        let meta = match auth_config.get(CONFIG_RESOURCE) {
            Some(resource) if !resource.trim().is_empty() => {
                let resource_url = Url::parse(resource)
                    .map_err(|e| DomainError::validation(format!("invalid 'resource' URL: {e}")))?;
                authcode::discover_from_resource(http, &resource_url)
                    .await
                    .map_err(|e| {
                        DomainError::internal(format!("authorization-server discovery failed: {e}"))
                    })?
            }
            _ => {
                let probe_url = probe_url_for_upstream(&upstream)?;
                authcode::discover_from_resource_challenge(http, &probe_url)
                    .await
                    .map_err(|e| {
                        DomainError::internal(format!(
                            "authorization-server discovery via WWW-Authenticate failed: {e}"
                        ))
                    })?
            }
        };

        // Intersect configured scopes with what the server advertises.
        let effective_scopes: Vec<String> = match meta.scopes_supported.as_ref() {
            Some(supported) if !scopes.is_empty() => scopes
                .into_iter()
                .filter(|s| supported.contains(s))
                .collect(),
            _ => scopes,
        };

        let registration_endpoint = meta.registration_endpoint.as_ref().ok_or_else(|| {
            DomainError::validation(
                "authorization server does not support dynamic client registration",
            )
        })?;
        let registered = authcode::register_client(
            http,
            registration_endpoint,
            &client_name,
            std::slice::from_ref(&redirect_uri),
            &effective_scopes,
        )
        .await
        .map_err(|e| DomainError::internal(format!("dynamic client registration failed: {e}")))?;

        let pkce = authcode::Pkce::generate();
        let state = authcode::generate_state();
        let authorization_url = authcode::build_authorize_url(
            &meta.authorization_endpoint,
            &registered.client_id,
            &redirect_uri,
            &effective_scopes,
            &state,
            &pkce.challenge,
        )
        .map_err(|e| DomainError::internal(format!("failed to build authorize URL: {e}")))?;

        let pending = PendingAuthorization {
            subject_id: ctx.subject_id(),
            subject_tenant_id: ctx.subject_tenant_id(),
            upstream_id,
            token_endpoint: meta.token_endpoint,
            client_id: registered.client_id,
            client_secret: registered.client_secret.map(|s| s.expose().to_owned()),
            code_verifier: pkce.verifier.expose().to_owned(),
            redirect_uri,
            scopes: effective_scopes,
            return_to,
        };
        self.pending.insert(state.clone(), pending);

        Ok(OAuthBeginOutcome {
            authorization_url: authorization_url.to_string(),
            state,
        })
    }

    async fn complete(&self, state: String, code: String) -> Result<String, DomainError> {
        let pending = self
            .pending
            .take(&state)
            .ok_or_else(|| DomainError::validation("unknown or expired authorization state"))?;

        let http = self.http().await?;
        let client_secret = pending
            .client_secret
            .as_ref()
            .map(|s| SecretString::new(s.clone()));
        let code_verifier = SecretString::new(pending.code_verifier.clone());
        let exchange = authcode::AuthCodeExchange {
            token_endpoint: &pending.token_endpoint,
            client_id: &pending.client_id,
            client_secret: client_secret.as_ref(),
            code: &code,
            redirect_uri: &pending.redirect_uri,
            code_verifier: &code_verifier,
        };
        let tokens = authcode::exchange_code(http, &exchange)
            .await
            .map_err(|e| {
                DomainError::internal(format!("authorization-code exchange failed: {e}"))
            })?;

        // The callback is unauthenticated; act on behalf of the subject that
        // began the flow, recovered from the pending state.
        let ctx = SecurityContext::builder()
            .subject_id(pending.subject_id)
            .subject_tenant_id(pending.subject_tenant_id)
            .build()
            .map_err(|e| DomainError::internal(format!("failed to build security context: {e}")))?;

        let record = OAuthTokenRecord {
            version: OAuthTokenRecord::CURRENT_VERSION,
            client_id: pending.client_id.clone(),
            client_secret: pending.client_secret.clone(),
            token_endpoint: pending.token_endpoint.clone(),
            access_token: tokens.access_token.expose().to_owned(),
            refresh_token: tokens.refresh_token.as_ref().map(|s| s.expose().to_owned()),
            expires_at_unix: now_unix()
                .saturating_add(i64::try_from(tokens.expires_in.as_secs()).unwrap_or(i64::MAX)),
            scope: tokens
                .scope
                .clone()
                .or_else(|| Some(pending.scopes.join(" ")).filter(|s| !s.is_empty())),
        };
        self.token_store
            .store(&ctx, pending.upstream_id, &record)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?;
        Ok(pending.return_to)
    }

    async fn abort(&self, state: String) -> Option<String> {
        // Single-use: remove the pending entry (matching `complete`) and hand
        // back its allowlisted `return_to` so the browser can be sent home.
        self.pending.take(&state).map(|pending| pending.return_to)
    }

    async fn revoke(&self, ctx: &SecurityContext, upstream_id: Uuid) -> Result<(), DomainError> {
        self.token_store
            .delete(ctx, upstream_id)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))
    }

    async fn status(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
    ) -> Result<OAuthConnectionStatus, DomainError> {
        let Some(record) = self
            .token_store
            .load(ctx, upstream_id)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?
        else {
            return Ok(OAuthConnectionStatus {
                connected: false,
                expires_at_unix: None,
            });
        };
        // Connected only when the access token is still valid or a refresh
        // token is available to obtain a new one.
        let connected = record.refresh_token.is_some() || record.expires_at_unix > now_unix();
        Ok(OAuthConnectionStatus {
            connected,
            expires_at_unix: Some(record.expires_at_unix),
        })
    }
}

#[cfg(test)]
#[path = "enrollment_tests.rs"]
mod enrollment_tests;
