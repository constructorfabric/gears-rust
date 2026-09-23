use super::*;

#[test]
fn config_defaults_are_applied() {
    let cfg: VaultCredStorePluginConfig = serde_saphyr::from_str("{}").expect("parse");
    assert_eq!(cfg.vendor, "openbao");
    assert_eq!(cfg.priority, 100);
    assert_eq!(cfg.address, "http://127.0.0.1:8200");
    assert!(cfg.token.expose().is_empty());
    assert_eq!(cfg.mount, "secret");
    assert_eq!(cfg.path_prefix, "credstore");
    assert_eq!(cfg.namespace, None);
    assert_eq!(cfg.timeout_secs, 5);
}

#[test]
fn config_accepts_explicit_values() {
    let yaml = r#"
vendor: "acme"
priority: 5
address: "http://vault.internal:8200"
token: "s.abc123"
mount: "kv"
path_prefix: "cred"
namespace: "team-a"
timeout_secs: 30
"#;
    let cfg: VaultCredStorePluginConfig = serde_saphyr::from_str(yaml).expect("parse");
    assert_eq!(cfg.vendor, "acme");
    assert_eq!(cfg.priority, 5);
    assert_eq!(cfg.address, "http://vault.internal:8200");
    assert_eq!(cfg.token.expose(), "s.abc123");
    assert_eq!(cfg.mount, "kv");
    assert_eq!(cfg.path_prefix, "cred");
    assert_eq!(cfg.namespace.as_deref(), Some("team-a"));
    assert_eq!(cfg.timeout_secs, 30);
}

#[test]
fn config_rejects_unknown_fields() {
    let yaml = r#"
vendor: "openbao"
unexpected: true
"#;
    let parsed: Result<VaultCredStorePluginConfig, _> = serde_saphyr::from_str(yaml);
    assert!(parsed.is_err());
}

#[test]
fn expand_vars_expands_token_placeholder() {
    use toolkit::var_expand::ExpandVars;
    // `std::env::set_var` is `unsafe` in edition 2024 and the workspace
    // forbids `unsafe_code`; `temp_env` scopes the mutation instead (see
    // `gears/system/cluster/cluster-sdk/src/wiring_tests.rs` for the same
    // pattern).
    temp_env::with_var(
        "VAULT_CREDSTORE_PLUGIN_TEST_TOKEN",
        Some("s.expanded-token"),
        || {
            let yaml = r#"token: "${VAULT_CREDSTORE_PLUGIN_TEST_TOKEN}""#;
            let mut cfg: VaultCredStorePluginConfig = serde_saphyr::from_str(yaml).expect("parse");
            cfg.expand_vars().expect("expand_vars should resolve");
            assert_eq!(cfg.token.expose(), "s.expanded-token");
        },
    );
}

#[test]
fn debug_does_not_leak_token() {
    let yaml = r#"token: "s.super-secret-token""#;
    let cfg: VaultCredStorePluginConfig = serde_saphyr::from_str(yaml).expect("parse");
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains("s.super-secret-token"));
    assert!(dbg.contains("<redacted>"));
}
