use super::*;

#[test]
fn vendor_can_be_overridden_via_serde() {
    let json = r#"{"vendor": "acme", "priority": 100}"#;
    let cfg: NoopUsageCollectorStorageConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.vendor, "acme");
}

#[test]
fn priority_can_be_overridden_via_serde() {
    let json = r#"{"vendor": "cyberfabric", "priority": 10}"#;
    let cfg: NoopUsageCollectorStorageConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.priority, 10);
}

#[test]
fn serde_default_applies_default_vendor_and_priority() {
    let cfg: NoopUsageCollectorStorageConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(
        cfg.vendor, "cyberfabric",
        "serde(default) must use Default impl"
    );
    assert_eq!(cfg.priority, 100, "serde(default) must use Default impl");
}

#[test]
fn rejects_unknown_fields() {
    let json = r#"{"vendor": "x", "priority": 1, "unexpected": true}"#;
    assert!(serde_json::from_str::<NoopUsageCollectorStorageConfig>(json).is_err());
}
