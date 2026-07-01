use super::*;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use chat_engine_sdk::models::{TenantId, UserId};

fn make_call_ctx(cfg: Option<JsonValue>, cancel: CancellationToken) -> PluginCallContext {
    PluginCallContext {
        request_id: Uuid::nil(),
        tenant_id: TenantId::new("t"),
        user_id: UserId::new("u"),
        plugin_instance_id: "p".into(),
        session_type_id: Uuid::nil(),
        plugin_config: cfg,
        enabled_capabilities: None,
        deadline: None,
        cancel,
    }
}

#[test]
fn extract_config_rejects_missing_endpoint() {
    let ctx = make_call_ctx(Some(serde_json::json!({})), CancellationToken::new());
    let err = WebhookCompatPlugin::extract_config(&ctx).unwrap_err();
    // We only assert on the *kind*; the message names the key.
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn extract_config_rejects_absent_config() {
    let ctx = make_call_ctx(None, CancellationToken::new());
    let err = WebhookCompatPlugin::extract_config(&ctx).unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn extract_config_parses_full_object() {
    let cfg = serde_json::json!({
        "endpoint": "https://example.invalid/webhook",
        "auth": { "bearer": true },
        "auth_value": "tok",
        "default_timeout_ms": 1500_u64,
    });
    let ctx = make_call_ctx(Some(cfg), CancellationToken::new());
    let parsed = WebhookCompatPlugin::extract_config(&ctx).expect("ok");
    assert_eq!(parsed.endpoint, "https://example.invalid/webhook");
    assert_eq!(parsed.default_timeout, Some(Duration::from_millis(1500)));
    assert_eq!(parsed.auth_value.as_deref(), Some("tok"));
}

#[test]
fn resolved_timeout_some_zero_returns_timeout() {
    let cfg = WebhookConfig {
        endpoint: "x".into(),
        auth_kind: None,
        auth_value: None,
        default_timeout: None,
    };
    let ctx = PluginCallContext {
        request_id: Uuid::nil(),
        tenant_id: TenantId::new("t"),
        user_id: UserId::new("u"),
        plugin_instance_id: "p".into(),
        session_type_id: Uuid::nil(),
        plugin_config: None,
        enabled_capabilities: None,
        deadline: Some(Instant::now().checked_sub(Duration::from_secs(1)).unwrap()),
        cancel: CancellationToken::new(),
    };
    let err = cfg.resolved_timeout(&ctx).unwrap_err();
    assert!(matches!(err, PluginError::Timeout { .. }));
}

#[test]
fn resolved_timeout_none_returns_default() {
    let cfg = WebhookConfig {
        endpoint: "x".into(),
        auth_kind: None,
        auth_value: None,
        default_timeout: Some(Duration::from_millis(750)),
    };
    let ctx = make_call_ctx(None, CancellationToken::new());
    let out = cfg.resolved_timeout(&ctx).expect("ok");
    assert_eq!(out, Duration::from_millis(750));
}

#[test]
fn resolved_timeout_none_and_no_config_falls_back_to_hard_ceiling() {
    // Pre-fix this combination returned `Ok(None)` → no per-request
    // timeout → a hung backend could wedge the outbound HTTP task
    // forever. The fix collapses the fallback chain into a positive
    // `Duration`, so the per-request timeout is always set.
    let cfg = WebhookConfig {
        endpoint: "x".into(),
        auth_kind: None,
        auth_value: None,
        default_timeout: None,
    };
    let ctx = make_call_ctx(None, CancellationToken::new());
    let out = cfg.resolved_timeout(&ctx).expect("ok");
    assert_eq!(
        out, DEFAULT_REQUEST_TIMEOUT,
        "missing deadline + missing config must fall back to the hard ceiling, \
         never to an unbounded request",
    );
}

#[test]
fn resolved_timeout_positive_forwards() {
    let cfg = WebhookConfig {
        endpoint: "x".into(),
        auth_kind: None,
        auth_value: None,
        default_timeout: None,
    };
    let mut ctx = make_call_ctx(None, CancellationToken::new());
    ctx.deadline = Some(Instant::now() + Duration::from_secs(5));
    let out = cfg.resolved_timeout(&ctx).expect("ok");
    assert!(out > Duration::from_secs(4) && out <= Duration::from_secs(5));
}

#[test]
fn auth_bearer_sets_authorization_header() {
    let cfg = WebhookConfig {
        endpoint: "x".into(),
        auth_kind: Some(serde_json::json!({ "bearer": true })),
        auth_value: Some("abc".into()),
        default_timeout: None,
    };
    let h = cfg.auth_headers().expect("ok");
    assert_eq!(
        h.get(reqwest::header::AUTHORIZATION)
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer abc"
    );
}

#[test]
fn auth_api_key_header_sets_custom_header() {
    let cfg = WebhookConfig {
        endpoint: "x".into(),
        auth_kind: Some(serde_json::json!({ "api_key_header": "X-Plugin-Key" })),
        auth_value: Some("secret".into()),
        default_timeout: None,
    };
    let h = cfg.auth_headers().expect("ok");
    assert_eq!(h.get("X-Plugin-Key").unwrap().to_str().unwrap(), "secret");
}

#[test]
fn auth_missing_value_errors() {
    let cfg = WebhookConfig {
        endpoint: "x".into(),
        auth_kind: Some(serde_json::json!({ "bearer": true })),
        auth_value: None,
        default_timeout: None,
    };
    let err = cfg.auth_headers().unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn status_mapping_table() {
    assert!(matches!(
        map_status_to_error(StatusCode::from_u16(401).unwrap(), None),
        PluginError::Unauthorized { .. }
    ));
    assert!(matches!(
        map_status_to_error(StatusCode::from_u16(404).unwrap(), None),
        PluginError::NotFound { .. }
    ));
    assert!(matches!(
        map_status_to_error(
            StatusCode::from_u16(429).unwrap(),
            Some(Duration::from_secs(7))
        ),
        PluginError::RateLimited { .. }
    ));
    assert!(matches!(
        map_status_to_error(StatusCode::from_u16(503).unwrap(), None),
        PluginError::Transient { .. }
    ));
    assert!(matches!(
        map_status_to_error(StatusCode::from_u16(400).unwrap(), None),
        PluginError::InvalidInput { .. }
    ));
}

#[test]
fn plugin_instance_id_returns_constructor_value() {
    let p = WebhookCompatPlugin::with_client("gts.cf.webhook.v1~vendor", Client::new());
    assert_eq!(p.plugin_instance_id(), "gts.cf.webhook.v1~vendor");
}

#[tokio::test]
async fn pre_flight_cancellation_short_circuits() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cfg = WebhookConfig {
        endpoint: "http://127.0.0.1:1".into(),
        auth_kind: None,
        auth_value: None,
        default_timeout: None,
    };
    let ctx = make_call_ctx(None, cancel);
    let err = post_json(&Client::new(), &cfg, &ctx, &serde_json::json!({}), "test")
        .await
        .unwrap_err();
    assert!(matches!(err, PluginError::Transient { .. }));
}
