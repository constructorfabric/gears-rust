use super::*;

#[test]
fn authorization_failed_constructor_sets_message() {
    let e = UsageCollectorError::authorization_failed("denied");
    assert!(
        matches!(e, UsageCollectorError::AuthorizationFailed { ref message } if message == "denied")
    );
}

#[test]
fn internal_constructor_sets_message() {
    let e = UsageCollectorError::internal("types-registry: boom");
    assert!(
        matches!(e, UsageCollectorError::Internal { ref message } if message == "types-registry: boom")
    );
}

#[test]
fn plugin_timeout_constructor() {
    let e = UsageCollectorError::plugin_timeout();
    assert!(matches!(e, UsageCollectorError::PluginTimeout));
}

#[test]
fn unavailable_constructor_sets_message() {
    let e = UsageCollectorError::unavailable("connection refused");
    assert!(
        matches!(e, UsageCollectorError::Unavailable { ref message } if message == "connection refused")
    );
}

// ── Display ───────────────────────────────────────────────────────

#[test]
fn display_authorization_failed() {
    let e = UsageCollectorError::authorization_failed("access denied");
    assert_eq!(e.to_string(), "authorization failed: access denied");
}

#[test]
fn display_internal() {
    let e = UsageCollectorError::internal("types-registry unavailable");
    assert_eq!(e.to_string(), "internal error: types-registry unavailable");
}

#[test]
fn display_plugin_timeout() {
    let e = UsageCollectorError::plugin_timeout();
    assert_eq!(e.to_string(), "storage plugin call timed out");
}

#[test]
fn display_unavailable() {
    let e = UsageCollectorError::unavailable("identity service down");
    assert_eq!(e.to_string(), "service unavailable: identity service down");
}

// ── is_retryable ─────────────────────────────────────────────────

#[test]
fn unavailable_is_retryable() {
    assert!(UsageCollectorError::unavailable("down").is_retryable());
}

#[test]
fn plugin_timeout_is_retryable() {
    assert!(UsageCollectorError::plugin_timeout().is_retryable());
}

#[test]
fn circuit_open_is_retryable() {
    assert!(UsageCollectorError::circuit_open().is_retryable());
}

#[test]
fn authorization_failed_is_not_retryable() {
    assert!(!UsageCollectorError::authorization_failed("denied").is_retryable());
}

#[test]
fn internal_is_not_retryable() {
    assert!(!UsageCollectorError::internal("boom").is_retryable());
}

#[test]
fn module_not_found_is_not_retryable() {
    assert!(!UsageCollectorError::module_not_found("my-module").is_retryable());
}
