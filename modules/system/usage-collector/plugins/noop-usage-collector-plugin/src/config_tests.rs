use super::*;

#[test]
fn configured_values_override_serde_defaults() {
    // Pair each field with a value distinct from its Default so the assertion fails if
    // serde silently falls back to the Default impl instead of using the configured value.
    let default_cfg = NoopUsageCollectorConfig::default();
    assert_ne!(default_cfg.vendor, "acme");
    assert_ne!(default_cfg.priority, 10);

    let json = r#"{"vendor": "acme", "priority": 10}"#;
    let cfg: NoopUsageCollectorConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.vendor, "acme");
    assert_eq!(cfg.priority, 10);
}

#[test]
fn serde_default_applies_default_vendor_and_priority() {
    let cfg: NoopUsageCollectorConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(
        cfg.vendor, "cyberfabric",
        "serde(default) must use Default impl"
    );
    assert_eq!(cfg.priority, 100, "serde(default) must use Default impl");
}

#[test]
fn rejects_unknown_fields() {
    let json = r#"{"vendor": "x", "priority": 1, "unexpected": true}"#;
    assert!(serde_json::from_str::<NoopUsageCollectorConfig>(json).is_err());
}
