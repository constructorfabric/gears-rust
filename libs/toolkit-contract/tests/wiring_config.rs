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

/// `rest_only_knobs_set` names exactly the pool/concurrency knobs that were
/// explicitly set — this is what the runtime warns on when a `grpc` transport
/// carries a REST-only knob that would silently go nowhere.
#[test]
fn rest_only_knobs_set_reports_which_were_set() {
    // None set on a bare grpc wiring -> empty.
    let w = parse(r#"{"transport": "grpc", "endpoint": "http://x:50051"}"#).unwrap();
    let ClientWiring::Grpc { tuning, .. } = w else {
        unreachable!()
    };
    assert!(tuning.rest_only_knobs_set().is_empty());

    // A subset set -> only those names, in declaration order.
    let w = parse(
        r#"{"transport": "grpc", "endpoint": "http://x:50051", "max_concurrent_requests": 10}"#,
    )
    .unwrap();
    let ClientWiring::Grpc { tuning, .. } = w else {
        unreachable!()
    };
    assert_eq!(
        tuning.rest_only_knobs_set(),
        vec!["max_concurrent_requests"]
    );

    // All three set -> all three names.
    let w = parse(
        r#"{
            "transport": "grpc",
            "endpoint": "http://x:50051",
            "pool_max_idle_per_host": 256,
            "pool_idle_timeout": "30s",
            "max_concurrent_requests": 500
        }"#,
    )
    .unwrap();
    let ClientWiring::Grpc { tuning, .. } = w else {
        unreachable!()
    };
    assert_eq!(
        tuning.rest_only_knobs_set(),
        vec![
            "pool_max_idle_per_host",
            "pool_idle_timeout",
            "max_concurrent_requests"
        ]
    );
}

/// `ConsumerWiring` — the consumer-side (`#[toolkit::consumes]`) schema:
/// an object with an optional `endpoint` (omit to keep discovery) plus flattened
/// `ClientTuning`. No `transport` tag (the consumer path is REST-only). The
/// bare-string form earlier drafts accepted is deliberately rejected.
mod consumer_wiring {
    use super::*;
    use toolkit_contract::wiring::ConsumerWiring;

    fn parse_consumer(json: &str) -> Result<ConsumerWiring, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn bare_string_is_rejected() {
        // The legacy bare-string escape hatch is no longer a valid shape — it
        // must fail loudly rather than be silently honoured or dropped. Use
        // `{ "endpoint": "..." }` instead.
        assert!(parse_consumer(r#""http://billing:8080""#).is_err());
    }

    #[test]
    fn object_form_with_endpoint_and_tuning() {
        let json = r#"{
            "endpoint": "http://billing:8080",
            "timeout": "5s",
            "max_concurrent_requests": 256,
            "pool_max_idle_per_host": 256
        }"#;
        let w = parse_consumer(json).expect("object form parses");
        let (endpoint, tuning) = w.into_parts();
        assert_eq!(endpoint.as_deref(), Some("http://billing:8080"));
        assert_eq!(tuning.timeout, Some(Duration::from_secs(5)));
        assert_eq!(tuning.max_concurrent_requests, Some(256));
        assert_eq!(tuning.pool_max_idle_per_host, Some(256));
    }

    #[test]
    fn object_form_without_endpoint_keeps_discovery() {
        // Omitting `endpoint` leaves discovery in place while still tuning.
        let w = parse_consumer(r#"{ "timeout": "1s", "max_concurrent_requests": 1 }"#)
            .expect("tuning-only object parses");
        let (endpoint, tuning) = w.into_parts();
        assert_eq!(endpoint, None, "no endpoint => discovery is untouched");
        assert_eq!(tuning.timeout, Some(Duration::from_secs(1)));
        assert_eq!(tuning.max_concurrent_requests, Some(1));
    }

    #[test]
    fn empty_object_is_all_defaults_and_no_endpoint() {
        let w = parse_consumer(r"{}").expect("empty object parses");
        let (endpoint, tuning) = w.into_parts();
        assert_eq!(endpoint, None);
        assert!(tuning.timeout.is_none());
        assert!(tuning.retry.is_none());
        assert!(tuning.max_concurrent_requests.is_none());
    }

    #[test]
    fn object_form_accepts_retry_overrides() {
        let json = r#"{ "endpoint": "http://x", "retry": { "max_attempts": 5 } }"#;
        let w = parse_consumer(json).expect("retry override parses");
        let (_endpoint, tuning) = w.into_parts();
        assert_eq!(tuning.retry.expect("retry present").max_attempts, Some(5));
    }

    #[test]
    fn a_non_object_is_rejected() {
        // Only an object is a valid `ConsumerWiring`; a list (or any scalar) is
        // a parse error.
        assert!(parse_consumer(r"[1, 2, 3]").is_err());
    }
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
        assert_eq!(cfg.pool_max_idle_per_host, 128);
        assert_eq!(cfg.pool_idle_timeout, Some(Duration::from_secs(90)));
        assert_eq!(cfg.max_concurrent_requests, Some(128));
    }

    #[test]
    fn tuning_apply_overrides_pool_and_concurrency() {
        let w = parse(
            r#"{
                "transport": "rest",
                "endpoint": "https://x",
                "pool_max_idle_per_host": 512,
                "pool_idle_timeout": "5m",
                "max_concurrent_requests": 1000
            }"#,
        )
        .unwrap();
        let ClientWiring::Rest { endpoint, tuning } = w else {
            unreachable!()
        };
        let cfg: ClientConfig = tuning.apply_to(endpoint);
        assert_eq!(cfg.pool_max_idle_per_host, 512);
        assert_eq!(cfg.pool_idle_timeout, Some(Duration::from_mins(5)));
        assert_eq!(cfg.max_concurrent_requests, Some(1000));
    }

    /// A `0` for either knob is accepted by `apply_to` and forwarded verbatim
    /// onto the `ClientConfig` — this test pins only that forwarding. Making a
    /// `0` cap safe is the transport's job (clamped to 1 in `toolkit-http`'s
    /// builder), proven behaviourally in the `concurrency_limit` integration
    /// test, not here.
    #[test]
    fn tuning_apply_accepts_zero_and_forwards_verbatim() {
        let w = parse(
            r#"{
                "transport": "rest",
                "endpoint": "https://x",
                "pool_max_idle_per_host": 0,
                "max_concurrent_requests": 0
            }"#,
        )
        .unwrap();
        let ClientWiring::Rest { endpoint, tuning } = w else {
            unreachable!()
        };
        let cfg: ClientConfig = tuning.apply_to(endpoint);
        assert_eq!(cfg.pool_max_idle_per_host, 0);
        assert_eq!(cfg.max_concurrent_requests, Some(0));
    }
}
