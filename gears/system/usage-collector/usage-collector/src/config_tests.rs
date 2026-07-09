//! Unit tests for the `[usage_collector]` configuration surface.
//!
//! Only the vendor binding and serde posture (`#[serde(default,
//! deny_unknown_fields)]`) are exercised here; the metric catalog is plugin-
//! owned under ADR-0012, so there is no host-side declared-catalog surface
//! left to test.

use super::*;

#[test]
fn serde_default_applies_default_vendor() {
    let cfg: UsageCollectorConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(
        cfg.vendor, "cyberfabric",
        "serde(default) must use Default impl"
    );
}

#[test]
fn vendor_can_be_overridden_via_serde() {
    let json = r#"{"vendor": "acme"}"#;
    let cfg: UsageCollectorConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.vendor, "acme");
}

#[test]
fn rejects_unknown_fields() {
    let json = r#"{"vendor": "x", "unexpected": true}"#;
    assert!(serde_json::from_str::<UsageCollectorConfig>(json).is_err());
}

#[test]
fn validate_accepts_default_vendor() {
    assert!(UsageCollectorConfig::default().validate().is_ok());
}

#[test]
fn validate_rejects_empty_vendor() {
    let cfg = UsageCollectorConfig {
        vendor: String::new(),
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn validate_rejects_whitespace_only_vendor() {
    let cfg = UsageCollectorConfig {
        vendor: "   \t ".to_owned(),
    };
    assert!(cfg.validate().is_err());
}
