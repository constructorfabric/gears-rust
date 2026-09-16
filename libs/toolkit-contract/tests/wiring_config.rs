//! Round-trip deserialization tests for [`ClientWiring`].
//!
//! Validates the public YAML/JSON shape consumed by `#[toolkit::provides]`:
//! the discriminator (`transport: local | rest | grpc`), the requirement
//! that `endpoint` is present on remote transports, and the
//! `humantime`-friendly tuning fields flattened into the same map.

use std::time::Duration;

use toolkit_contract::wiring::{ClientWiring, RetrySettings};

/// Helper: deserialize via `serde_json` (YAML-equivalent for the shapes here).
fn parse(json: &str) -> Result<ClientWiring, serde_json::Error> {
    serde_json::from_str(json)
}

#[test]
fn local_transport_has_no_fields() {
    let w = parse(r#"{"transport": "local"}"#).expect("local should parse");
    assert!(matches!(w, ClientWiring::Local));
}

#[test]
fn local_rejects_endpoint() {
    // Internally-tagged `Local` is a unit variant — extra keys are tolerated
    // by default but `endpoint` on `local` is meaningless. We document the
    // current behaviour: extra keys are ignored. If we want strict deny, the
    // serde attribute would need `deny_unknown_fields`, which doesn't compose
    // with `flatten` used by the remote variants.
    let w = parse(r#"{"transport": "local", "endpoint": "x"}"#).expect("local ignores endpoint");
    assert!(matches!(w, ClientWiring::Local));
}

#[test]
fn rest_requires_endpoint() {
    let err = parse(r#"{"transport": "rest"}"#).expect_err("missing endpoint should error");
    assert!(
        err.to_string().contains("endpoint"),
        "error mentions endpoint: {err}"
    );
}

#[test]
fn rest_with_endpoint_only() {
    let w = parse(r#"{"transport": "rest", "endpoint": "https://x.example"}"#)
        .expect("rest+endpoint parses");
    let ClientWiring::Rest { endpoint, tuning } = w else {
        panic!("expected Rest variant");
    };
    assert_eq!(endpoint, "https://x.example");
    assert!(tuning.timeout.is_none());
    assert!(tuning.retry.is_none());
}

#[test]
fn rest_with_humantime_timeout() {
    let w = parse(r#"{"transport": "rest", "endpoint": "https://x", "timeout": "5s"}"#)
        .expect("humantime timeout parses");
    let ClientWiring::Rest { tuning, .. } = w else {
        unreachable!()
    };
    assert_eq!(tuning.timeout, Some(Duration::from_secs(5)));
}

#[test]
fn rest_with_retry_overrides() {
    let json = r#"{
        "transport": "rest",
        "endpoint": "https://x",
        "retry": { "max_attempts": 5, "base_delay": "200ms", "multiplier": 1.5 }
    }"#;
    let w = parse(json).expect("retry overrides parse");
    let ClientWiring::Rest { tuning, .. } = w else {
        unreachable!()
    };
    let RetrySettings {
        max_attempts,
        base_delay,
        max_delay,
        multiplier,
    } = tuning.retry.expect("retry present");
    assert_eq!(max_attempts, Some(5));
    assert_eq!(base_delay, Some(Duration::from_millis(200)));
    assert_eq!(max_delay, None);
    assert_eq!(multiplier, Some(1.5));
}

/// Parse a grpc wiring document whose reconnect tuning is spelled `key`, and
/// return that tuning.
///
/// Stream reconnect tuning is meaningless for grpc transport at runtime but the
/// schema is shared — accepting it here is a no-op rather than a parse failure.
/// Documents current shape.
#[allow(
    clippy::expect_used,
    reason = "Test-only helper: a document that fails to parse, or a tuning field that lands nowhere, IS the failure each caller is asserting against, and the expect message names which. `allow-expect-in-tests` covers `#[test]` fns but not a shared helper like this one."
)]
fn grpc_reconnect_under_key(key: &str) -> toolkit_contract::wiring::ReconnectSettings {
    let json = format!(
        r#"{{
        "transport": "grpc",
        "endpoint": "http://payments:50051",
        "{key}": {{ "max_attempts": 3, "base_delay": "1s" }}
    }}"#
    );
    let w = parse(&json).expect("grpc parses with stream tuning");
    let ClientWiring::Grpc { endpoint, tuning } = w else {
        panic!("expected Grpc variant");
    };
    assert_eq!(endpoint, "http://payments:50051");
    tuning.stream_reconnect.expect("stream_reconnect present")
}

#[test]
fn grpc_with_stream_reconnect() {
    let settings = grpc_reconnect_under_key("stream_reconnect");
    assert_eq!(settings.max_attempts, Some(3));
    assert_eq!(settings.base_delay, Some(Duration::from_secs(1)));
}

/// #4734 Q10: `stream_reconnect` was called `sse_reconnect` before the policy
/// covered framings other than SSE, and `ClientTuning` has no `rename_all`, so
/// the Rust field name *is* the JSON key. The `#[serde(alias)]` is what keeps
/// an already-deployed config file working across the rename, and this is the
/// test that makes it mean something: both spellings must land on the same
/// field with the same value.
#[test]
fn legacy_sse_reconnect_key_still_deserializes() {
    let legacy = grpc_reconnect_under_key("sse_reconnect");
    let current = grpc_reconnect_under_key("stream_reconnect");
    assert_eq!(legacy.max_attempts, current.max_attempts);
    assert_eq!(legacy.base_delay, current.base_delay);
    assert_eq!(legacy.max_delay, current.max_delay);
    assert_eq!(legacy.max_attempts, Some(3));
    assert_eq!(legacy.base_delay, Some(Duration::from_secs(1)));
}

/// #4740 #9: `stream_reconnect` and its legacy alias `sse_reconnect` name the
/// *same* field, so a config that supplies **both** is rejected as a duplicate
/// field rather than silently honouring one and dropping the other. This holds
/// even though `ClientTuning` is `#[serde(flatten)]`-ed into `ClientWiring`:
/// serde's flatten still detects a field populated twice via name + alias. The
/// outcome is order-independent and the error names the canonical field.
///
/// (Empirically verified — an earlier note guessed flatten would bypass
/// duplicate detection and resolve silently; it does not.)
#[test]
fn both_stream_reconnect_and_its_alias_is_a_duplicate_field_error() {
    for json in [
        r#"{ "transport": "grpc", "endpoint": "http://x:50051",
             "stream_reconnect": { "max_attempts": 11 },
             "sse_reconnect": { "max_attempts": 22 } }"#,
        r#"{ "transport": "grpc", "endpoint": "http://x:50051",
             "sse_reconnect": { "max_attempts": 22 },
             "stream_reconnect": { "max_attempts": 11 } }"#,
    ] {
        let err = parse(json).expect_err("both keys present must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate field") && msg.contains("stream_reconnect"),
            "expected a duplicate-field error naming stream_reconnect, got: {msg}"
        );
    }
}

#[test]
fn default_wiring_is_local() {
    let w = ClientWiring::default();
    assert!(matches!(w, ClientWiring::Local));
}

#[cfg(feature = "runtime-client")]
mod runtime_conversion {
    use super::*;
    use toolkit_contract::runtime::config::ClientConfig;

    #[test]
    fn tuning_apply_overrides_timeout_and_retry() {
        let w = parse(
            r#"{
                "transport": "rest",
                "endpoint": "https://x",
                "timeout": "2s",
                "retry": { "max_attempts": 7 }
            }"#,
        )
        .unwrap();
        let ClientWiring::Rest { endpoint, tuning } = w else {
            unreachable!()
        };
        let cfg: ClientConfig = tuning.apply_to(endpoint);
        assert_eq!(cfg.base_url, "https://x");
        assert_eq!(cfg.timeout, Duration::from_secs(2));
        assert_eq!(cfg.retry.max_attempts, 7);
        // Untouched fields stay at runtime defaults.
        assert_eq!(cfg.retry.base_delay, Duration::from_millis(100));
    }

    #[test]
    fn tuning_can_require_tls() {
        // A wired client forwards the tenant bearer token over whatever scheme
        // the endpoint uses, so a deployment talking outside a trusted boundary
        // needs a way to demand TLS. Without this knob the wiring always got
        // the permissive default.
        let w = parse(
            r#"{
                "transport": "rest",
                "endpoint": "https://x",
                "require_tls": true
            }"#,
        )
        .unwrap();
        let ClientWiring::Rest { endpoint, tuning } = w else {
            unreachable!()
        };
        assert!(tuning.apply_to(endpoint).require_tls);
    }

    #[test]
    fn tuning_apply_with_no_overrides_keeps_defaults() {
        let w = parse(r#"{"transport": "rest", "endpoint": "https://y"}"#).unwrap();
        let ClientWiring::Rest { endpoint, tuning } = w else {
            unreachable!()
        };
        let cfg = tuning.apply_to(endpoint);
        assert_eq!(cfg.timeout, Duration::from_secs(30));
        assert_eq!(cfg.retry.max_attempts, 3);
    }
}
