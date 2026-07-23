use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use httpmock::prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::*;
use crate::domain::plugin::AuthContext;
use crate::infra::oauth::token_store::{TokenStoreError, UserTokenStore};

fn test_http() -> toolkit_http::HttpClientConfig {
    toolkit_http::HttpClientConfig::for_testing()
}

/// Auth context for the fixed test upstream. The token store derives its own
/// key from `(subject, upstream_id)`, so tests only need a stable `upstream_id`
/// and the optional `resource` re-auth hint.
fn test_ctx() -> AuthContext {
    let mut config = HashMap::new();
    config.insert(
        "resource".to_owned(),
        "https://mcp.example.com/mcp".to_owned(),
    );
    AuthContext {
        headers: HashMap::new(),
        config,
        security_context: SecurityContext::builder()
            .subject_id(Uuid::nil())
            .subject_tenant_id(Uuid::nil())
            .build()
            .unwrap(),
        upstream_id: Uuid::nil(),
    }
}

fn record(expires_at: i64, refresh: Option<&str>, token_endpoint: &str) -> OAuthTokenRecord {
    record_with("at-current", expires_at, refresh, token_endpoint)
}

fn record_with(
    access_token: &str,
    expires_at: i64,
    refresh: Option<&str>,
    token_endpoint: &str,
) -> OAuthTokenRecord {
    OAuthTokenRecord {
        version: OAuthTokenRecord::CURRENT_VERSION,
        client_id: "client-1".to_owned(),
        client_secret: None,
        token_endpoint: token_endpoint.to_owned(),
        access_token: access_token.to_owned(),
        refresh_token: refresh.map(String::from),
        expires_at_unix: expires_at,
        scope: None,
    }
}

// ---------------------------------------------------------------------------
// UserTokenStore test doubles
// ---------------------------------------------------------------------------

/// Stateful token store: `load` returns the current record, `store` overwrites
/// it, `delete` clears it — so tests can assert what was persisted.
struct StatefulTokenStore {
    record: parking_lot::Mutex<Option<OAuthTokenRecord>>,
}

impl StatefulTokenStore {
    fn new(initial: Option<OAuthTokenRecord>) -> Self {
        Self {
            record: parking_lot::Mutex::new(initial),
        }
    }

    fn current(&self) -> Option<OAuthTokenRecord> {
        self.record.lock().clone()
    }
}

#[async_trait::async_trait]
impl UserTokenStore for StatefulTokenStore {
    async fn load(
        &self,
        _ctx: &SecurityContext,
        _upstream_id: Uuid,
    ) -> Result<Option<OAuthTokenRecord>, TokenStoreError> {
        Ok(self.record.lock().clone())
    }

    async fn store(
        &self,
        _ctx: &SecurityContext,
        _upstream_id: Uuid,
        record: &OAuthTokenRecord,
    ) -> Result<(), TokenStoreError> {
        *self.record.lock() = Some(record.clone());
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        _upstream_id: Uuid,
    ) -> Result<(), TokenStoreError> {
        *self.record.lock() = None;
        Ok(())
    }
}

/// Token store whose `load` returns successive queued records (the last entry
/// repeats), letting a test simulate a record changing between reads. `delete`
/// is counted so tests can assert clobber-avoidance.
struct SequencedTokenStore {
    responses: parking_lot::Mutex<VecDeque<OAuthTokenRecord>>,
    delete_count: AtomicUsize,
}

impl SequencedTokenStore {
    fn new(responses: Vec<OAuthTokenRecord>) -> Self {
        Self {
            responses: parking_lot::Mutex::new(responses.into()),
            delete_count: AtomicUsize::new(0),
        }
    }

    fn deletes(&self) -> usize {
        self.delete_count.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl UserTokenStore for SequencedTokenStore {
    async fn load(
        &self,
        _ctx: &SecurityContext,
        _upstream_id: Uuid,
    ) -> Result<Option<OAuthTokenRecord>, TokenStoreError> {
        let mut q = self.responses.lock();
        let record = if q.len() > 1 {
            q.pop_front()
        } else {
            q.front().cloned()
        };
        Ok(record)
    }

    async fn store(
        &self,
        _ctx: &SecurityContext,
        _upstream_id: Uuid,
        _record: &OAuthTokenRecord,
    ) -> Result<(), TokenStoreError> {
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        _upstream_id: Uuid,
    ) -> Result<(), TokenStoreError> {
        self.delete_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn plugin_with(initial: Option<OAuthTokenRecord>) -> OAuth2AuthCodeAuthPlugin {
    let store = Arc::new(StatefulTokenStore::new(initial));
    OAuth2AuthCodeAuthPlugin::with_http_config(store, test_http())
}

// ---------------------------------------------------------------------------
// Read / refresh behavior
// ---------------------------------------------------------------------------

#[tokio::test]
async fn valid_token_injects_bearer() {
    let plugin = plugin_with(Some(record(
        now_unix() + 3600,
        Some("rt"),
        "https://unused.example.com/token",
    )));
    let mut ctx = test_ctx();

    plugin.authenticate(&mut ctx).await.unwrap();
    assert_eq!(
        ctx.headers.get("authorization").unwrap(),
        "Bearer at-current"
    );
}

#[tokio::test]
async fn missing_record_requires_authorization() {
    let plugin = plugin_with(None);
    let mut ctx = test_ctx();

    let err = plugin.authenticate(&mut ctx).await.unwrap_err();
    assert!(matches!(err, PluginError::AuthorizationRequired(_)));
}

#[tokio::test]
async fn expired_without_refresh_requires_authorization() {
    let plugin = plugin_with(Some(record(
        now_unix() - 10,
        None,
        "https://unused.example.com/token",
    )));
    let mut ctx = test_ctx();

    let err = plugin.authenticate(&mut ctx).await.unwrap_err();
    assert!(matches!(err, PluginError::AuthorizationRequired(_)));
}

#[tokio::test]
async fn expired_with_refresh_injects_new_bearer() {
    let server = MockServer::start();
    let token_ep = format!("http://localhost:{}/token", server.port());
    let m = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .body_includes("grant_type=refresh_token")
            .body_includes("refresh_token=rt-old");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"at-new","refresh_token":"rt-new","expires_in":3600,"token_type":"Bearer"}"#);
    });

    let store = Arc::new(StatefulTokenStore::new(Some(record(
        now_unix() - 10,
        Some("rt-old"),
        &token_ep,
    ))));
    let plugin = OAuth2AuthCodeAuthPlugin::with_http_config(store.clone(), test_http());
    let mut ctx = test_ctx();

    plugin.authenticate(&mut ctx).await.unwrap();
    assert_eq!(ctx.headers.get("authorization").unwrap(), "Bearer at-new");
    m.assert();

    // The rotated record must be persisted back: the old refresh token is
    // replaced by the newly issued one (rt-new), not left as rt-old.
    let stored = store.current().expect("record persisted");
    assert_eq!(stored.access_token, "at-new");
    assert_eq!(stored.refresh_token.as_deref(), Some("rt-new"));
}

#[tokio::test]
async fn refresh_rejected_requires_authorization() {
    let server = MockServer::start();
    let token_ep = format!("http://localhost:{}/token", server.port());
    let _m = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(400)
            .header("content-type", "application/json")
            .body(r#"{"error":"invalid_grant"}"#);
    });

    let plugin = plugin_with(Some(record(now_unix() - 10, Some("rt-old"), &token_ep)));
    let mut ctx = test_ctx();

    let err = plugin.authenticate(&mut ctx).await.unwrap_err();
    assert!(matches!(err, PluginError::AuthorizationRequired(_)));
}

// ---------------------------------------------------------------------------
// Token record: redaction and versioning
// ---------------------------------------------------------------------------

#[test]
fn debug_output_redacts_secret_material() {
    let record = OAuthTokenRecord {
        version: OAuthTokenRecord::CURRENT_VERSION,
        client_id: "client-1".to_owned(),
        client_secret: Some("super-secret".to_owned()),
        token_endpoint: "https://as.example.com/token".to_owned(),
        access_token: "live-access-token".to_owned(),
        refresh_token: Some("live-refresh-token".to_owned()),
        expires_at_unix: 1_000,
        scope: Some("read write".to_owned()),
    };

    let dbg = format!("{record:?}");

    assert!(!dbg.contains("super-secret"), "client_secret leaked: {dbg}");
    assert!(
        !dbg.contains("live-access-token"),
        "access_token leaked: {dbg}"
    );
    assert!(
        !dbg.contains("live-refresh-token"),
        "refresh_token leaked: {dbg}"
    );
    assert!(dbg.contains("[REDACTED]"));
    assert!(dbg.contains("client-1"));
    assert!(dbg.contains("as.example.com"));
}

#[test]
fn from_slice_defaults_missing_version_to_one() {
    // Legacy records written before versioning carry no `version` field and
    // must still deserialize, defaulting to version 1.
    let legacy = r#"{"client_id":"c","token_endpoint":"https://as/t","access_token":"at","expires_at_unix":0}"#;
    let record = OAuthTokenRecord::from_slice(legacy.as_bytes()).unwrap();
    assert_eq!(record.version, OAuthTokenRecord::CURRENT_VERSION);
}

#[test]
fn from_slice_rejects_newer_version() {
    let future = r#"{"version":999,"client_id":"c","token_endpoint":"https://as/t","access_token":"at","expires_at_unix":0}"#;
    let err = OAuthTokenRecord::from_slice(future.as_bytes()).unwrap_err();
    assert!(
        err.contains("unsupported token record version 999"),
        "err: {err}"
    );
}

// ---------------------------------------------------------------------------
// Concurrent-refresh serialization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reload_under_lock_reuses_concurrent_refresh() {
    // Pre-lock read sees an expired token; the under-lock re-read sees a record
    // already renewed by a concurrent refresh, so we reuse it without making a
    // network refresh call (the token endpoint is intentionally unreachable).
    let expired = record_with(
        "at-old",
        now_unix() - 10,
        Some("rt"),
        "http://127.0.0.1:1/token",
    );
    let renewed = record_with(
        "at-renewed",
        now_unix() + 3600,
        Some("rt"),
        "http://127.0.0.1:1/token",
    );
    let store = Arc::new(SequencedTokenStore::new(vec![expired, renewed]));
    let plugin = OAuth2AuthCodeAuthPlugin::with_http_config(store.clone(), test_http());
    let mut ctx = test_ctx();

    plugin.authenticate(&mut ctx).await.unwrap();
    assert_eq!(
        ctx.headers.get("authorization").unwrap(),
        "Bearer at-renewed"
    );
    assert_eq!(store.deletes(), 0);
}

#[tokio::test]
async fn refresh_reject_skips_delete_when_record_changed() {
    let server = MockServer::start();
    let token_ep = format!("http://localhost:{}/token", server.port());
    server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(400)
            .header("content-type", "application/json")
            .body(r#"{"error":"invalid_grant"}"#);
    });

    // Pre-lock and under-lock reads return the stale record (refresh -> reject);
    // the pre-delete re-read returns a newer record written out-of-band, so the
    // stale-cleanup delete must be skipped.
    let stale = record_with("at-stale", now_unix() - 10, Some("rt-old"), &token_ep);
    let newer = record_with("at-fresh", now_unix() + 3600, Some("rt-fresh"), &token_ep);
    let store = Arc::new(SequencedTokenStore::new(vec![stale.clone(), stale, newer]));
    let plugin = OAuth2AuthCodeAuthPlugin::with_http_config(store.clone(), test_http());
    let mut ctx = test_ctx();

    let err = plugin.authenticate(&mut ctx).await.unwrap_err();
    assert!(matches!(err, PluginError::AuthorizationRequired(_)));
    assert_eq!(store.deletes(), 0, "must not clobber a newer record");
}

#[tokio::test]
async fn refresh_reject_deletes_when_record_unchanged() {
    let server = MockServer::start();
    let token_ep = format!("http://localhost:{}/token", server.port());
    server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(400)
            .header("content-type", "application/json")
            .body(r#"{"error":"invalid_grant"}"#);
    });

    // The record is unchanged across all reads, so the stale record is deleted.
    let stale = record_with("at-stale", now_unix() - 10, Some("rt-old"), &token_ep);
    let store = Arc::new(SequencedTokenStore::new(vec![
        stale.clone(),
        stale.clone(),
        stale,
    ]));
    let plugin = OAuth2AuthCodeAuthPlugin::with_http_config(store.clone(), test_http());
    let mut ctx = test_ctx();

    let err = plugin.authenticate(&mut ctx).await.unwrap_err();
    assert!(matches!(err, PluginError::AuthorizationRequired(_)));
    assert_eq!(store.deletes(), 1);
}
