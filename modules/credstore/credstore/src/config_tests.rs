// Created: 2026-04-07 by Constructor Tech
use super::*;

#[test]
fn vendor_can_be_overridden_via_serde() {
    let json = r#"{"vendor": "acme"}"#;
    let cfg: CredStoreConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.vendor, "acme");
}

#[test]
fn serde_default_applies_default_vendor() {
    let cfg: CredStoreConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(
        cfg.vendor, "cyberfabric",
        "serde(default) must use Default impl"
    );
}

#[test]
fn rejects_unknown_fields() {
    let json = r#"{"vendor": "x", "unexpected": true}"#;
    assert!(serde_json::from_str::<CredStoreConfig>(json).is_err());
}
