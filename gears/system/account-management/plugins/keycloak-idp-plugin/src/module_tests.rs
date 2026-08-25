//! Regression coverage for [`pre_warm_bootstrap_admin_with_retry`].
//!
//! These tests pin the three behaviours the operator runbook depends on:
//!
//! 1. **Fast-bail on permanent KC failures** (4xx-non-429) — bad
//!    `client_secret` / missing realm MUST NOT eat the full retry budget.
//! 2. **Recovery on transient failures** — a single 5xx followed by 200
//!    succeeds within the budget, and the success-path tags `attempt > 1`
//!    so the "succeeded after retry" log carries the right count.
//! 3. **Exhausted budget annotation** — when all attempts fail, the
//!    anyhow error message carries the attempt total so post-mortems can
//!    distinguish "single-shot failure" from "burned the whole budget".
//!
//! The transport's own `[http_retry_policy].max_retries` is set to `0` so
//! transient-status retries surface to `pre_warm` instead of being absorbed
//! one layer down. Backoff is dialled to 1ms so the test suite stays fast.

use super::{is_pre_warm_retryable, pre_warm_bootstrap_admin_with_retry};
use crate::config::{
    BootstrapAdminConfig, BootstrapPreWarmConfig, HttpRetryPolicy, KeycloakConfig,
    KeycloakMetricsConfig, RealmAdminConfig, SecretFromEnv,
};
use crate::domain::credstore::CredStoreReader;
use crate::domain::error::{KcStatusKind, PluginError};
use crate::domain::kc::factory::KeycloakAdminClientFactory;
use crate::domain::system_actor::build_system_ctx;
use crate::domain::test_support::NoopMetrics;
use crate::domain::test_support::StubCredStore;
use crate::infra::kc_http::ReqwestKcTransport;
use credstore_sdk::CredStoreClientV1;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `KeycloakConfig` with transport-level retries disabled so test
/// behaviour is driven by `pre_warm`'s own loop, not by the inner
/// reqwest retry budget.
fn cfg_no_transport_retries(base: &str) -> KeycloakConfig {
    KeycloakConfig {
        base_url: url::Url::parse(base).expect("valid base url"),
        bootstrap_admin: BootstrapAdminConfig {
            realm: "master".into(),
            client_id: "keycloak-idp-plugin-bootstrap".into(),
            client_secret: SecretFromEnv::from_literal_unchecked("topsecret"),
        },
        realm_admin: RealmAdminConfig {
            client_id_default: "keycloak-idp-plugin-realm-admin".into(),
            default_shared_realm_secret: SecretFromEnv::from_literal_unchecked("ra"),
            secret_ref_template: "keycloak-idp-realm-admin-{realm_name}-secret".into(),
        },
        tls_ca_bundle_ref: None,
        admin_token_lifetime_safety_ms: 30_000,
        http_request_timeout_ms: 5_000,
        credential_refresh_interval: None,
        http_retry_policy: HttpRetryPolicy {
            max_retries: 0,
            ..HttpRetryPolicy::default()
        },
        metrics: KeycloakMetricsConfig::default(),
        bootstrap_pre_warm: BootstrapPreWarmConfig::default(),
    }
}

fn factory_for(server: &MockServer) -> KeycloakAdminClientFactory {
    let cfg = cfg_no_transport_retries(&server.uri());
    let stub: Arc<dyn CredStoreClientV1> = Arc::new(StubCredStore);
    let reader = CredStoreReader::new(stub, build_system_ctx(Uuid::nil()));
    // `ReqwestKcTransport::new` defaults to `HttpRetryPolicy::default()`
    // (max_retries=3). The pre_warm tests need transport-level retries
    // disabled so the count we observe at wiremock is driven purely by
    // `pre_warm`'s own loop — use `with_retry_policy` to plumb through
    // the test cfg's zero-retry policy.
    let transport = Arc::new(ReqwestKcTransport::with_retry_policy(
        reqwest::Client::new(),
        cfg.http_retry_policy.clone(),
    ));
    KeycloakAdminClientFactory::new(cfg, transport, reader, Arc::new(NoopMetrics))
}

const TOKEN_PATH: &str = "/realms/master/protocol/openid-connect/token";

// -----------------------------------------------------------------
// Unit coverage for the classifier itself — guards against silent
// behaviour drift if a future hand adds a new `PluginError` variant.
// -----------------------------------------------------------------

#[test]
fn is_pre_warm_retryable_classifies_kc_statuses() {
    let mk = |status| PluginError::KcRest {
        method: "POST",
        path_template: "/realms/{realm}/protocol/openid-connect/token".into(),
        status,
        body_first_2kb: String::new(),
    };
    // 4xx-non-429 → permanent.
    assert!(!is_pre_warm_retryable(&mk(KcStatusKind::Http(400))));
    assert!(!is_pre_warm_retryable(&mk(KcStatusKind::Http(401))));
    assert!(!is_pre_warm_retryable(&mk(KcStatusKind::Http(403))));
    assert!(!is_pre_warm_retryable(&mk(KcStatusKind::Http(404))));
    // 429 + 5xx → transient.
    assert!(is_pre_warm_retryable(&mk(KcStatusKind::Http(429))));
    assert!(is_pre_warm_retryable(&mk(KcStatusKind::Http(500))));
    assert!(is_pre_warm_retryable(&mk(KcStatusKind::Http(503))));
    // Transport / timeout always transient.
    assert!(is_pre_warm_retryable(&mk(KcStatusKind::Transport)));
    assert!(is_pre_warm_retryable(&mk(KcStatusKind::Timeout)));
    // CredStoreRead transient — OpenBao readiness lag is a real
    // deploy-ordering race keycloak-idp pods hit in practice.
    assert!(is_pre_warm_retryable(&PluginError::CredStoreRead {
        detail: "openbao 503".into(),
    }));
    // Config errors are operator misconfig — never retryable.
    assert!(!is_pre_warm_retryable(&PluginError::Config {
        detail: "bad base url".into(),
    }));
}

// -----------------------------------------------------------------
// (a) 4xx-non-429 fast-bails on the first attempt.
// -----------------------------------------------------------------

#[tokio::test]
async fn pre_warm_fast_bails_on_4xx_non_429() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(move |_req: &wiremock::Request| {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            // 401 invalid_client — the canonical broken-bootstrap-secret
            // shape. Retrying this is guaranteed to reproduce.
            ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "invalid_client",
                "error_description": "Invalid client or Invalid client credentials"
            }))
        })
        .mount(&server)
        .await;

    let factory = factory_for(&server);
    let err = pre_warm_bootstrap_admin_with_retry(&factory, 5, Duration::from_millis(1))
        .await
        .expect_err("401 must surface as error");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "permanent 4xx MUST fast-bail on the first attempt - no retry budget burn",
    );
    // Error message carries the attempt count (1, not the configured 5)
    // so post-mortems distinguish fast-bail from exhausted-budget.
    let msg = format!("{err}");
    assert!(
        msg.contains("1/5"),
        "error message must record the actual attempt count, got: {msg}",
    );
    assert!(
        msg.contains("401"),
        "error message must surface the underlying status, got: {msg}",
    );
}

// -----------------------------------------------------------------
// (b) Transient-then-success returns Ok after exactly 2 attempts.
//     The `attempt > 1` branch in the loop emits the
//     "succeeded after retry" INFO log; here we assert the observable
//     side of that branch (call count + Ok result).
// -----------------------------------------------------------------

#[tokio::test]
async fn pre_warm_recovers_from_transient_then_success() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(move |_req: &wiremock::Request| {
            let n = calls_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First attempt: KC still warming up.
                ResponseTemplate::new(503).set_body_string("Service Unavailable")
            } else {
                // Second attempt: ready.
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "tok",
                    "expires_in": 600
                }))
            }
        })
        .mount(&server)
        .await;

    let factory = factory_for(&server);
    pre_warm_bootstrap_admin_with_retry(&factory, 5, Duration::from_millis(1))
        .await
        .expect("transient-then-success must return Ok");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "exactly one retry must be issued (1 x 503 + 1 x 200)",
    );
}

// -----------------------------------------------------------------
// (c) Exhausted budget on persistent transient — final error message
//     carries the attempt total.
// -----------------------------------------------------------------

#[tokio::test]
async fn pre_warm_exhausts_budget_on_persistent_transient() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(move |_req: &wiremock::Request| {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(503).set_body_string("Service Unavailable")
        })
        .mount(&server)
        .await;

    let factory = factory_for(&server);
    let err = pre_warm_bootstrap_admin_with_retry(&factory, 5, Duration::from_millis(1))
        .await
        .expect_err("budget exhausted must surface as error");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        5,
        "all 5 attempts must be issued before bailing",
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("5/5"),
        "error message must record exhausted-budget attempt total, got: {msg}",
    );
    assert!(
        msg.contains("503"),
        "error message must surface the underlying status, got: {msg}",
    );
}
