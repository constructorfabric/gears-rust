// Created: 2026-04-07 by Constructor Tech
use super::*;

#[test]
fn config_defaults_are_applied() {
    let yaml = r#"
secrets:
  - tenant_id: "00000000-0000-0000-0000-000000000001"
    owner_id: "00000000-0000-0000-0000-000000000002"
    key: "openai_api_key"
    value: "sk-test-123"
"#;

    let cfg: StaticCredStorePluginConfig = serde_saphyr::from_str(yaml).unwrap();

    assert_eq!(cfg.vendor, "cyberfabric");
    assert_eq!(cfg.priority, 100);
    assert_eq!(cfg.secrets.len(), 1);
    assert!(cfg.secrets[0].sharing.is_none());
    assert_eq!(cfg.secrets[0].resolve_sharing(), SharingMode::Private);
}

#[test]
fn config_allows_omitted_tenant_and_owner() {
    let yaml = r#"
secrets:
  - key: "global_api_key"
    value: "sk-global"
"#;

    let cfg: StaticCredStorePluginConfig = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(cfg.secrets.len(), 1);
    assert!(cfg.secrets[0].tenant_id.is_none());
    assert!(cfg.secrets[0].owner_id.is_none());
    assert!(cfg.secrets[0].sharing.is_none());
    assert_eq!(cfg.secrets[0].resolve_sharing(), SharingMode::Shared);
}

#[test]
fn config_allows_partial_tenant_only() {
    let yaml = r#"
secrets:
  - tenant_id: "00000000-0000-0000-0000-000000000001"
    key: "scoped_key"
    value: "val"
"#;

    let cfg: StaticCredStorePluginConfig = serde_saphyr::from_str(yaml).unwrap();
    assert!(cfg.secrets[0].tenant_id.is_some());
    assert!(cfg.secrets[0].owner_id.is_none());
    assert!(cfg.secrets[0].sharing.is_none());
    assert_eq!(cfg.secrets[0].resolve_sharing(), SharingMode::Tenant);
}

#[test]
fn config_explicit_sharing_overrides_default() {
    // tenant_id + no owner_id defaults to Tenant; override to Shared.
    let yaml = r#"
secrets:
  - tenant_id: "00000000-0000-0000-0000-000000000001"
    key: "key"
    value: "val"
    sharing: "shared"
"#;

    let cfg: StaticCredStorePluginConfig = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(cfg.secrets[0].sharing, Some(SharingMode::Shared));
    assert_eq!(cfg.secrets[0].resolve_sharing(), SharingMode::Shared);
}

#[test]
fn config_rejects_unknown_fields() {
    let yaml = r#"
vendor: "cyberfabric"
priority: 100
unexpected: true
"#;

    let parsed: Result<StaticCredStorePluginConfig, _> = serde_saphyr::from_str(yaml);
    assert!(parsed.is_err());
}

#[test]
fn config_allows_empty_secrets() {
    let parsed: Result<StaticCredStorePluginConfig, _> = serde_saphyr::from_str("{}");
    assert!(parsed.is_ok());

    let cfg = match parsed {
        Ok(cfg) => cfg,
        Err(e) => panic!("failed to parse config: {e}"),
    };
    assert!(cfg.secrets.is_empty());
    assert_eq!(cfg.vendor, "cyberfabric");
    assert_eq!(cfg.priority, 100);
}
