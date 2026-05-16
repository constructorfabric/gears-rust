use url::Url;

use super::{UsageCollectorRestClientConfig, is_insecure_non_loopback_http};

const CLIENT_ID: &str = "CLIENT_ID";
const CLIENT_SECRET: &str = "CLIENT_SECRET";

fn valid_cfg_json() -> serde_json::Value {
    serde_json::json!({
        "collector_url": "http://127.0.0.1:8080",
        "oauth": {
            "client_id": CLIENT_ID,
            "client_secret": CLIENT_SECRET
        }
    })
}

#[test]
fn collector_url_is_parsed_as_url() {
    let json = serde_json::json!({
        "collector_url": "http://collector:9090",
        "oauth": {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET}
    });
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(json).unwrap();
    assert_eq!(cfg.collector_url.host_str(), Some("collector"));
    assert_eq!(cfg.collector_url.port(), Some(9090));
}

#[test]
fn scopes_default_to_empty() {
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(valid_cfg_json()).unwrap();
    assert!(cfg.oauth.scopes.is_empty());
}

#[test]
fn scopes_can_be_set_via_serde() {
    let json = serde_json::json!({
        "collector_url": "http://127.0.0.1:8080",
        "oauth": {
            "client_id": CLIENT_ID,
            "client_secret": CLIENT_SECRET,
            "scopes": ["read:usage", "write:usage"]
        }
    });
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(json).unwrap();
    assert_eq!(cfg.oauth.scopes, ["read:usage", "write:usage"]);
}

#[test]
fn collector_url_is_required() {
    let json = serde_json::json!({
        "oauth": {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET}
    });
    assert!(serde_json::from_value::<UsageCollectorRestClientConfig>(json).is_err());
}

#[test]
fn client_id_is_required() {
    let json = serde_json::json!({
        "collector_url": "http://127.0.0.1:8080",
        "oauth": {"client_secret": CLIENT_SECRET}
    });
    assert!(serde_json::from_value::<UsageCollectorRestClientConfig>(json).is_err());
}

#[test]
fn client_secret_is_required() {
    let json = serde_json::json!({
        "collector_url": "http://127.0.0.1:8080",
        "oauth": {"client_id": CLIENT_ID}
    });
    assert!(serde_json::from_value::<UsageCollectorRestClientConfig>(json).is_err());
}

#[test]
fn rejects_unknown_fields() {
    let json = serde_json::json!({
        "collector_url": "http://127.0.0.1:8080",
        "oauth": {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET},
        "extra": true
    });
    assert!(serde_json::from_value::<UsageCollectorRestClientConfig>(json).is_err());
}

// validate: collector_url checks

#[test]
fn validate_rejects_non_hierarchical_collector_url() {
    // "cannot-be-a-base" (opaque) URLs like data: have no host or path hierarchy.
    let json = serde_json::json!({
        "collector_url": "data:text/plain,hello",
        "oauth": {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET}
    });
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(json).unwrap();
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("hierarchical"),
        "error must mention hierarchical URL, got: {err}"
    );
}

// validate: S2S credential checks

#[test]
fn validate_rejects_empty_client_id() {
    let json = serde_json::json!({
        "collector_url": "http://127.0.0.1:8080",
        "oauth": {"client_id": "", "client_secret": CLIENT_SECRET}
    });
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(json).unwrap();
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("client_id"),
        "error must mention client_id, got: {err}"
    );
}

#[test]
fn validate_rejects_whitespace_only_client_id() {
    let json = serde_json::json!({
        "collector_url": "http://127.0.0.1:8080",
        "oauth": {"client_id": "   ", "client_secret": CLIENT_SECRET}
    });
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(json).unwrap();
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("client_id"),
        "error must mention client_id, got: {err}"
    );
}

#[test]
fn validate_rejects_empty_client_secret() {
    let json = serde_json::json!({
        "collector_url": "http://127.0.0.1:8080",
        "oauth": {"client_id": CLIENT_ID, "client_secret": ""}
    });
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(json).unwrap();
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("client_secret"),
        "error must mention client_secret, got: {err}"
    );
}

#[test]
fn validate_rejects_whitespace_only_client_secret() {
    let json = serde_json::json!({
        "collector_url": "http://127.0.0.1:8080",
        "oauth": {"client_id": CLIENT_ID, "client_secret": "   "}
    });
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(json).unwrap();
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("client_secret"),
        "error must mention client_secret, got: {err}"
    );
}

#[test]
fn validate_accepts_valid_credentials() {
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(valid_cfg_json()).unwrap();
    assert!(cfg.validate().is_ok());
}

#[test]
fn debug_output_redacts_oauth_credentials() {
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(valid_cfg_json()).unwrap();
    let debug = format!("{cfg:?}");
    assert!(
        !debug.contains(CLIENT_ID),
        "Debug output must not contain client_id value, got: {debug}"
    );
    assert!(
        !debug.contains(CLIENT_SECRET),
        "Debug output must not contain client_secret value, got: {debug}"
    );
    assert!(
        debug.contains("[REDACTED]"),
        "Debug output must contain [REDACTED], got: {debug}"
    );
}

// cpt-cf-dod-rest-ingest-tls-config: TLS/HTTPS startup check
//
// Two-layer coverage:
//   1. `is_insecure_non_loopback_http_predicate` — table-driven unit check of the
//      helper itself (one test, one input list).
//   2. `validate_emits_warn_for_insecure_non_loopback_http` — end-to-end check
//      that `validate()` actually emits a WARN event for an insecure config.
//      This is the test the DoD signs off on; the helper-only check would still
//      pass if `validate()` silently dropped its warning.

#[test]
fn is_insecure_non_loopback_http_predicate() {
    // (url, expected_insecure)
    let cases: &[(&str, bool)] = &[
        // non-loopback http → insecure
        ("http://example.com", true),
        ("http://example.com:8080", true),
        // loopback hostnames / addresses → not flagged
        ("http://localhost", false),
        ("http://localhost:8080", false),
        ("http://LocalHost:8080", false), // case-insensitive
        ("http://localhost.", false),     // trailing-dot FQDN
        ("http://localhost.:8080", false),
        ("http://foo.localhost", false), // RFC 6761 § 6.3 subdomain
        ("http://foo.bar.localhost:8080", false),
        ("http://foo.localhost.", false), // subdomain with trailing dot
        // names containing "localhost" as a non-tail label are NOT loopback
        ("http://localhost.example.com", true),
        ("http://notlocalhost", true),
        ("http://127.0.0.1:8080", false),
        ("http://127.0.0.2:8080", false),
        ("http://127.0.0.255:8080", false),
        ("http://127.255.255.1:8080", false), // full 127.0.0.0/8 per RFC 1122
        ("http://[::1]:8080", false),         // IPv6 loopback
        // non-loopback IPv6 → insecure (pin the `!address.is_loopback()` branch in
        // the `Host::Ipv6(_)` arm — a regression that hard-coded `false` there
        // would be caught here).
        ("http://[2001:db8::1]:8080", true),
        // https is always allowed
        ("https://example.com", false),
        ("https://collector.internal:443", false),
    ];

    for (raw, expected) in cases {
        let url = Url::parse(raw).unwrap();
        assert_eq!(
            is_insecure_non_loopback_http(&url),
            *expected,
            "{raw}: expected insecure={expected}",
        );
    }
}

#[test]
#[tracing_test::traced_test]
fn validate_emits_warn_for_insecure_non_loopback_http() {
    // cpt-cf-dod-rest-ingest-tls-config
    // Drives the real `validate()` path so a regression that drops the WARN
    // (or accepts http://example.com silently) is caught.
    let json = serde_json::json!({
        "collector_url": "http://example.com",
        "oauth": {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET}
    });
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(json).unwrap();
    cfg.validate()
        .expect("validate must accept (warn, not fail) http://example.com");

    // tracing-test's `logs_contain` matches a substring across all captured
    // events for the current test; pin both the URL and the "http://" marker
    // so an unrelated info/debug line cannot satisfy the assertion alone.
    assert!(
        logs_contain("http://example.com"),
        "validate() must emit a WARN mentioning the insecure collector_url",
    );
}

#[test]
#[tracing_test::traced_test]
fn validate_does_not_warn_for_https() {
    let json = serde_json::json!({
        "collector_url": "https://collector.internal",
        "oauth": {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET}
    });
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(json).unwrap();
    cfg.validate().unwrap();

    assert!(
        !logs_contain("collector_url uses http://"),
        "validate() must not warn when the collector_url is https://",
    );
}

#[test]
#[tracing_test::traced_test]
fn validate_does_not_warn_for_http_loopback() {
    let cfg: UsageCollectorRestClientConfig = serde_json::from_value(valid_cfg_json()).unwrap();
    cfg.validate().unwrap();

    assert!(
        !logs_contain("collector_url uses http://"),
        "validate() must not warn for the loopback default config",
    );
}
