//! D-221 input bounds, run against the production rules: the well-known token prefixes refused
//! in proof and external references, the per-order limiter key-store ceiling and the overlap
//! scope key length.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup must stop the test at once; `allow`, not `expect`, \
              because not every test in the file unwraps or expects"
)]
use bss_orders::config::OrdersConfig;
use bss_orders::domain::audit::carries_credential;
use bss_orders::domain::overlap::{ClaimRuleError, OverlapScopeKey};
use serde_json::json;

/// Every well-known prefix with a token body is a credential, at the start of the reference or
/// after a separator; the same prefix without a body, inside a word or as an ordinary
/// reference is not.
#[test]
fn well_known_token_prefixes_are_credentials() {
    let body = "A1b2C3d4E5f6G7h8I9j0";
    for prefix in [
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "glpat-",
        "sk_live_",
        "sk_test_",
        "rk_live_",
        "xoxa-",
        "xoxb-",
        "xoxp-",
        "xoxr-",
    ] {
        let token = format!("{prefix}{body}");
        assert!(carries_credential(&token), "{token}");
        assert!(carries_credential(&format!("proof:{token}")), "{token}");
        assert!(carries_credential(&format!("urn:x/{token}")), "{token}");
        assert!(!carries_credential(prefix), "{prefix} alone");
        assert!(
            !carries_credential(&format!("{prefix}short")),
            "{prefix} short body"
        );
        assert!(
            !carries_credential(&format!("x{token}")),
            "{prefix} inside a word"
        );
    }
    for aws in ["AKIAIOSFODNN7EXAMPLE", "ASIAIOSFODNN7EXAMPLE"] {
        assert!(carries_credential(aws), "{aws}");
        assert!(carries_credential(&format!("ref:{aws}")), "{aws}");
        // Not exactly 16 upper-case characters: not an access key id.
        assert!(!carries_credential(&format!("{aws}X")), "{aws} too long");
        assert!(!carries_credential(&aws[..19]), "{aws} too short");
        assert!(!carries_credential(&aws.to_ascii_lowercase()));
    }
    let google = "AIzaSyA-1234567890abcdefghijklmnopqrstu";
    assert!(carries_credential(google));
    assert!(!carries_credential(&google[..30]));
    for reference in [
        "proof:abc",
        "urn:am:delegation:123e4567",
        "https://issuer.example/proofs/42",
        "a.b.c",
        "ghp-is-not-a-prefix-0123456789abcdef",
        "task_live_0123456789abcdef0123",
        "AKIA",
    ] {
        assert!(!carries_credential(reference), "{reference}");
    }
}

fn config(max_keys: u64) -> OrdersConfig {
    serde_json::from_value(json!({
        "lock_route": "direct",
        "idempotency_lease_seconds": 30,
        "dependency_timeout_ms": 2000,
        "service_principals": [{
            "role": "workflow",
            "subject_id": "00000000-0000-0000-0000-000000000901",
            "tenant_id": "00000000-0000-0000-0000-000000000001"
        }],
        "events": {
            "producer_subject_id": "00000000-0000-0000-0000-00000000044c",
            "producer_tenant_id": "00000000-0000-0000-0000-00000000044d"
        },
        "audit_minimization": {"key_id": "test-1", "key": "unit-test-key-0123456789abcdefgh"},
        "throttling": {"per_order_max_keys": max_keys}
    }))
    .unwrap()
}

/// `per_order_max_keys` is the only memory bound on caller-keyed limiter state: 1..=100000, the
/// baseline being the maximum; anything else fails boot.
#[test]
fn the_limiter_key_bound_is_capped_at_its_baseline() {
    for admitted in [1, 50_000, 100_000] {
        config(admitted).validate().unwrap();
    }
    for refused in [0, 100_001, u64::MAX] {
        let error = config(refused).validate().unwrap_err().to_string();
        assert!(error.contains("per_order_max_keys"), "{refused}: {error}");
    }
}

/// An overlap scope key is at most 1024 UTF-8 bytes; longer is `InvalidKey`, never a raw index
/// error at the claim insert.
#[test]
fn overlap_scope_keys_are_bounded_in_bytes() {
    assert_eq!(OverlapScopeKey::MAX_KEY_BYTES, 1024);
    let at_bound = "k".repeat(1024);
    assert_eq!(
        OverlapScopeKey::try_from(at_bound.clone())
            .unwrap()
            .as_str(),
        at_bound
    );
    assert_eq!(
        OverlapScopeKey::try_from("k".repeat(1025)),
        Err(ClaimRuleError::InvalidKey)
    );
    // The bound is bytes, not characters: 342 three-byte characters are 1026 bytes.
    assert_eq!(
        OverlapScopeKey::try_from("\u{20AC}".repeat(342)),
        Err(ClaimRuleError::InvalidKey)
    );
    assert!(OverlapScopeKey::try_from("\u{20AC}".repeat(341)).is_ok());
}
