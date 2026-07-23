use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
// Only the discovery-driven `begin` tests use `httpmock`, and those are gated
// out under `--features fips` (plaintext transport). Gate the import too so it
// is not flagged as unused in FIPS builds.
#[cfg(not(feature = "fips"))]
use httpmock::prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::*;
use crate::domain::model;
use crate::infra::oauth::token_store::TokenStoreError;

fn test_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .build()
        .unwrap()
}

// ---------------------------------------------------------------------------
// In-memory UserTokenStore keyed by upstream_id
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MapTokenStore {
    records: Mutex<HashMap<Uuid, OAuthTokenRecord>>,
}

impl MapTokenStore {
    fn with(entries: Vec<(Uuid, OAuthTokenRecord)>) -> Self {
        Self {
            records: Mutex::new(entries.into_iter().collect()),
        }
    }

    // Only asserted by `begin_then_complete_persists_token_record`, which is
    // gated out under `--features fips`; gate the helper to avoid dead code.
    #[cfg(not(feature = "fips"))]
    fn get(&self, upstream_id: Uuid) -> Option<OAuthTokenRecord> {
        self.records.lock().unwrap().get(&upstream_id).cloned()
    }

    fn contains(&self, upstream_id: Uuid) -> bool {
        self.records.lock().unwrap().contains_key(&upstream_id)
    }
}

#[async_trait]
impl UserTokenStore for MapTokenStore {
    async fn load(
        &self,
        _ctx: &SecurityContext,
        upstream_id: Uuid,
    ) -> Result<Option<OAuthTokenRecord>, TokenStoreError> {
        Ok(self.records.lock().unwrap().get(&upstream_id).cloned())
    }

    async fn store(
        &self,
        _ctx: &SecurityContext,
        upstream_id: Uuid,
        record: &OAuthTokenRecord,
    ) -> Result<(), TokenStoreError> {
        self.records
            .lock()
            .unwrap()
            .insert(upstream_id, record.clone());
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        upstream_id: Uuid,
    ) -> Result<(), TokenStoreError> {
        self.records.lock().unwrap().remove(&upstream_id);
        Ok(())
    }
}

fn token_record(access_token: &str, refresh: Option<&str>, expires_at: i64) -> OAuthTokenRecord {
    OAuthTokenRecord {
        version: OAuthTokenRecord::CURRENT_VERSION,
        client_id: "c".to_owned(),
        client_secret: None,
        token_endpoint: "http://localhost/token".to_owned(),
        access_token: access_token.to_owned(),
        refresh_token: refresh.map(String::from),
        expires_at_unix: expires_at,
        scope: None,
    }
}

// ---------------------------------------------------------------------------
// Minimal ControlPlaneService: only get_upstream is meaningful
// ---------------------------------------------------------------------------

struct MockCp {
    upstream: Option<model::Upstream>,
}

/// Deployment-configured OAGW callback URL used as the OAuth `redirect_uri`.
const TEST_CALLBACK_URL: &str = "https://gw.example.com/oagw/v1/oauth/callback";
/// Allowlisted post-callback `return_to` used by the tests.
const TEST_RETURN_TO: &str = "https://app.example.com/connected";

fn upstream_with_auth(resource: &str) -> model::Upstream {
    upstream_with_auth_scopes(resource, "")
}

fn upstream_with_auth_scopes(resource: &str, scopes: &str) -> model::Upstream {
    let mut config = HashMap::new();
    config.insert(CONFIG_RESOURCE.to_owned(), resource.to_owned());
    if !scopes.is_empty() {
        config.insert(CONFIG_SCOPES.to_owned(), scopes.to_owned());
    }
    model::Upstream {
        id: Uuid::nil(),
        tenant_id: Uuid::nil(),
        alias: "corp-mcp".to_owned(),
        server: model::Server { endpoints: vec![] },
        protocol: "http".to_owned(),
        enabled: true,
        auth: Some(model::AuthConfig {
            plugin_type: "oauth2_auth_code".to_owned(),
            sharing: model::SharingMode::Private,
            config: Some(config),
        }),
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
    }
}

#[async_trait]
impl ControlPlaneService for MockCp {
    async fn create_upstream(
        &self,
        _ctx: &SecurityContext,
        _req: model::CreateUpstreamRequest,
    ) -> Result<model::Upstream, DomainError> {
        unimplemented!()
    }

    async fn get_upstream(
        &self,
        _ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<model::Upstream, DomainError> {
        self.upstream
            .clone()
            .ok_or_else(|| DomainError::not_found("upstream", id))
    }

    async fn list_upstreams(
        &self,
        _ctx: &SecurityContext,
        _query: &model::ListQuery,
    ) -> Result<Vec<model::Upstream>, DomainError> {
        unimplemented!()
    }

    async fn update_upstream(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _req: model::UpdateUpstreamRequest,
    ) -> Result<model::Upstream, DomainError> {
        unimplemented!()
    }

    async fn delete_upstream(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<Vec<Uuid>, DomainError> {
        unimplemented!()
    }

    async fn create_route(
        &self,
        _ctx: &SecurityContext,
        _req: model::CreateRouteRequest,
    ) -> Result<model::Route, DomainError> {
        unimplemented!()
    }

    async fn get_route(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<model::Route, DomainError> {
        unimplemented!()
    }

    async fn list_routes(
        &self,
        _ctx: &SecurityContext,
        _upstream_id: Option<Uuid>,
        _query: &model::ListQuery,
    ) -> Result<Vec<model::Route>, DomainError> {
        unimplemented!()
    }

    async fn update_route(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _req: model::UpdateRouteRequest,
    ) -> Result<model::Route, DomainError> {
        unimplemented!()
    }

    async fn delete_route(&self, _ctx: &SecurityContext, _id: Uuid) -> Result<(), DomainError> {
        unimplemented!()
    }

    async fn resolve_proxy_target(
        &self,
        _ctx: &SecurityContext,
        _alias: &str,
        _method: &str,
        _path: &str,
    ) -> Result<(model::Upstream, model::Route), DomainError> {
        unimplemented!()
    }
}

fn test_http() -> toolkit_http::HttpClientConfig {
    toolkit_http::HttpClientConfig::for_testing()
}

/// Mount the discovery + registration endpoints on `server`, pointing at
/// itself as both resource origin and issuer.
///
/// Only used by the discovery-driven `begin` tests, which are gated out under
/// `--features fips`; gate the helper too so it is not dead code there.
#[cfg(not(feature = "fips"))]
fn mount_discovery(server: &MockServer) {
    let base = format!("http://localhost:{}", server.port());
    server.mock(|when, then| {
        // RFC 9728: the well-known segment is inserted after the authority and
        // the resource path (`/mcp`) is preserved.
        when.method(GET)
            .path("/.well-known/oauth-protected-resource/mcp");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"{{"authorization_servers":["{base}"]}}"#));
    });
    server.mock(|when, then| {
        when.method(GET)
            .path("/.well-known/oauth-authorization-server");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"issuer":"{base}","authorization_endpoint":"{base}/authorize","token_endpoint":"{base}/token","registration_endpoint":"{base}/register","scopes_supported":["read","write"]}}"#
            ));
    });
    server.mock(|when, then| {
        when.method(POST).path("/register");
        then.status(201)
            .header("content-type", "application/json")
            .body(r#"{"client_id":"dcr-client-1"}"#);
    });
}

fn service_with(
    upstream: Option<model::Upstream>,
    token_store: Arc<dyn UserTokenStore>,
) -> OAuthEnrollmentServiceImpl {
    service_full(
        upstream,
        token_store,
        Some(TEST_CALLBACK_URL.to_owned()),
        vec![TEST_RETURN_TO.to_owned()],
    )
}

fn service_full(
    upstream: Option<model::Upstream>,
    token_store: Arc<dyn UserTokenStore>,
    oauth_callback_url: Option<String>,
    return_to_allowlist: Vec<String>,
) -> OAuthEnrollmentServiceImpl {
    let cp: Arc<dyn ControlPlaneService> = Arc::new(MockCp { upstream });
    let pending = Arc::new(PendingAuthorizationStore::new(Duration::from_secs(600)));
    OAuthEnrollmentServiceImpl::with_http_config(
        cp,
        token_store,
        pending,
        oauth_callback_url,
        return_to_allowlist,
        test_http(),
    )
}

// Skipped under `--features fips`: this test stands up an `httpmock` plaintext
// server and builds the discovery/registration client via
// `HttpClientConfig::for_testing()` (`AllowInsecureHttp`), which `toolkit-http`
// rejects at `build()` with `HttpError::InsecureTransport` under FIPS — see
// PR #1985. The same gate applies to the other discovery-driven `begin` tests.
#[cfg(not(feature = "fips"))]
#[tokio::test]
async fn begin_returns_authorization_url() {
    let server = MockServer::start();
    mount_discovery(&server);
    let resource = format!("http://localhost:{}/mcp", server.port());

    let svc = service_with(
        Some(upstream_with_auth(&resource)),
        Arc::new(MapTokenStore::default()),
    );

    let outcome = svc
        .begin(
            &test_ctx(),
            Uuid::nil(),
            TEST_RETURN_TO.to_owned(),
            "mini-chat".to_owned(),
        )
        .await
        .unwrap();

    assert!(outcome.authorization_url.contains("/authorize"));
    assert!(
        outcome
            .authorization_url
            .contains(&format!("state={}", outcome.state))
    );
    assert!(outcome.authorization_url.contains("client_id=dcr-client-1"));
}

// Skipped under `--features fips` (plaintext `httpmock` discovery) — see the
// note on `begin_returns_authorization_url` and PR #1985.
#[cfg(not(feature = "fips"))]
#[tokio::test]
async fn begin_filters_unsupported_scopes_from_authorize_url() {
    let server = MockServer::start();
    // Discovery advertises scopes_supported = ["read", "write"].
    mount_discovery(&server);
    let resource = format!("http://localhost:{}/mcp", server.port());

    // Scopes are sourced from the upstream auth config, not the caller: config
    // requests a supported scope and an unadvertised one; only the supported
    // scope must survive the intersection and reach the authorize URL.
    let svc = service_with(
        Some(upstream_with_auth_scopes(&resource, "read admin")),
        Arc::new(MapTokenStore::default()),
    );

    let outcome = svc
        .begin(
            &test_ctx(),
            Uuid::nil(),
            TEST_RETURN_TO.to_owned(),
            "mini-chat".to_owned(),
        )
        .await
        .unwrap();

    assert!(
        outcome.authorization_url.contains("scope=read"),
        "expected supported scope in URL: {}",
        outcome.authorization_url
    );
    assert!(
        !outcome.authorization_url.contains("admin"),
        "unsupported scope leaked into URL: {}",
        outcome.authorization_url
    );
}

// Skipped under `--features fips` (plaintext `httpmock` discovery + token
// exchange) — see the note on `begin_returns_authorization_url` and PR #1985.
#[cfg(not(feature = "fips"))]
#[tokio::test]
async fn begin_then_complete_persists_token_record() {
    let server = MockServer::start();
    mount_discovery(&server);
    server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .body_includes("grant_type=authorization_code")
            .body_includes("code=auth-code-xyz")
            // Bind the exchange to the PKCE verifier and redirect_uri; without
            // these the request would still match even if either were dropped.
            .body_includes("code_verifier=")
            .body_includes("redirect_uri=");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3600,"token_type":"Bearer"}"#);
    });
    let resource = format!("http://localhost:{}/mcp", server.port());

    let token_store = Arc::new(MapTokenStore::default());
    let svc = service_with(Some(upstream_with_auth(&resource)), token_store.clone());

    let outcome = svc
        .begin(
            &test_ctx(),
            Uuid::nil(),
            TEST_RETURN_TO.to_owned(),
            "mini-chat".to_owned(),
        )
        .await
        .unwrap();

    // The callback carries no SecurityContext: complete recovers the acting
    // subject from the pending state resolved by `state`, and returns the
    // allowlisted `return_to` captured at begin.
    let return_to = svc
        .complete(outcome.state.clone(), "auth-code-xyz".to_owned())
        .await
        .unwrap();
    assert_eq!(return_to, TEST_RETURN_TO);

    let record = token_store
        .get(Uuid::nil())
        .expect("token record persisted");
    assert_eq!(record.access_token, "at-1");
    assert_eq!(record.refresh_token.as_deref(), Some("rt-1"));

    // Pending state is single-use: a second callback with the same state fails.
    let err = svc
        .complete(outcome.state, "auth-code-xyz".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn complete_unknown_state_is_validation_error() {
    let svc = service_with(
        Some(upstream_with_auth("http://localhost/mcp")),
        Arc::new(MapTokenStore::default()),
    );
    let err = svc
        .complete("does-not-exist".to_owned(), "code".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn status_reports_connected_when_refresh_token_present() {
    // Access token already expired, but a refresh token keeps it usable.
    let token_store = Arc::new(MapTokenStore::with(vec![(
        Uuid::nil(),
        token_record("at", Some("rt"), 1_000),
    )]));
    let svc = service_with(
        Some(upstream_with_auth("http://localhost/mcp")),
        token_store,
    );

    let status = svc.status(&test_ctx(), Uuid::nil()).await.unwrap();
    assert!(status.connected);
    assert_eq!(status.expires_at_unix, Some(1_000));
}

#[tokio::test]
async fn status_reports_disconnected_when_expired_without_refresh() {
    let token_store = Arc::new(MapTokenStore::with(vec![(
        Uuid::nil(),
        token_record("at", None, 1_000),
    )]));
    let svc = service_with(
        Some(upstream_with_auth("http://localhost/mcp")),
        token_store,
    );

    let status = svc.status(&test_ctx(), Uuid::nil()).await.unwrap();
    assert!(!status.connected);
    assert_eq!(status.expires_at_unix, Some(1_000));
}

#[tokio::test]
async fn status_reports_disconnected_when_absent() {
    let svc = service_with(
        Some(upstream_with_auth("http://localhost/mcp")),
        Arc::new(MapTokenStore::default()),
    );
    let status = svc.status(&test_ctx(), Uuid::nil()).await.unwrap();
    assert!(!status.connected);
    assert_eq!(status.expires_at_unix, None);
}

#[tokio::test]
async fn revoke_deletes_token_record() {
    let token_store = Arc::new(MapTokenStore::with(vec![(
        Uuid::nil(),
        token_record("at", Some("rt"), 1_000),
    )]));
    let svc = service_with(
        Some(upstream_with_auth("http://localhost/mcp")),
        token_store.clone(),
    );

    svc.revoke(&test_ctx(), Uuid::nil()).await.unwrap();
    assert!(!token_store.contains(Uuid::nil()));
}

#[tokio::test]
async fn begin_rejects_return_to_not_in_allowlist() {
    let svc = service_with(
        Some(upstream_with_auth("https://example.com/mcp")),
        Arc::new(MapTokenStore::default()),
    );
    // A `return_to` outside the deployment allowlist is rejected before any
    // upstream lookup or network call.
    let err = svc
        .begin(
            &test_ctx(),
            Uuid::nil(),
            "https://evil.example.com/steal".to_owned(),
            "mini-chat".to_owned(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn begin_without_callback_url_config_is_error() {
    // The OAuth `redirect_uri` is deployment config, never caller-supplied;
    // when unset the flow cannot proceed.
    let svc = service_full(
        Some(upstream_with_auth("https://example.com/mcp")),
        Arc::new(MapTokenStore::default()),
        None,
        vec![TEST_RETURN_TO.to_owned()],
    );
    let err = svc
        .begin(
            &test_ctx(),
            Uuid::nil(),
            TEST_RETURN_TO.to_owned(),
            "mini-chat".to_owned(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Internal { .. }));
}

#[tokio::test]
async fn begin_missing_upstream_is_not_found() {
    let svc = service_with(None, Arc::new(MapTokenStore::default()));
    let err = svc
        .begin(
            &test_ctx(),
            Uuid::nil(),
            TEST_RETURN_TO.to_owned(),
            "mini-chat".to_owned(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotFound { .. }));
}

#[test]
fn pending_authorization_debug_redacts_secret_material() {
    let pending = PendingAuthorization {
        subject_id: Uuid::nil(),
        subject_tenant_id: Uuid::nil(),
        upstream_id: Uuid::nil(),
        token_endpoint: "https://as.example.com/token".to_owned(),
        client_id: "client-1".to_owned(),
        client_secret: Some("super-secret".to_owned()),
        code_verifier: "pkce-verifier-xyz".to_owned(),
        redirect_uri: "https://app.example.com/callback".to_owned(),
        scopes: vec!["read".to_owned()],
        return_to: "https://app.example.com/connected".to_owned(),
    };

    let dbg = format!("{pending:?}");

    assert!(!dbg.contains("super-secret"), "client_secret leaked: {dbg}");
    assert!(
        !dbg.contains("pkce-verifier-xyz"),
        "code_verifier leaked: {dbg}"
    );
    assert!(dbg.contains("[REDACTED]"));
    // Non-sensitive fields remain for diagnostics.
    assert!(dbg.contains("client-1"));
    assert!(dbg.contains("as.example.com"));
}
