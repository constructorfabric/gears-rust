// Created: 2026-09-23 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
use super::*;

/// A configuration that passes `validate`: the defaults plus an inline token.
fn valid() -> VaultCredStorePluginConfig {
    VaultCredStorePluginConfig {
        token: Some(VaultToken::from("s.token")),
        ..VaultCredStorePluginConfig::default()
    }
}

fn parse(yaml: &str) -> VaultCredStorePluginConfig {
    serde_saphyr::from_str(yaml).expect("parse")
}

/// The validation error text, for a configuration that must be rejected.
fn rejection(cfg: &VaultCredStorePluginConfig) -> String {
    cfg.validate().expect_err("must be rejected").to_string()
}

#[test]
fn config_defaults_are_applied() {
    let cfg = parse("{}");
    assert_eq!(cfg.vendor, "openbao");
    assert_eq!(cfg.priority, 100);
    assert_eq!(cfg.address, "http://127.0.0.1:8200");
    assert!(cfg.token.is_none());
    assert!(cfg.token_file.is_none());
    assert!(cfg.token_env.is_none());
    assert_eq!(cfg.mount, "secret");
    assert_eq!(cfg.path_prefix, "credstore");
    assert_eq!(cfg.namespace, None);
    assert_eq!(cfg.timeout_secs, 5);
    assert_eq!(cfg.retry.max_attempts, 3);
    assert_eq!(cfg.retry.base_delay_ms, 100);
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
retry:
  max_attempts: 5
  base_delay_ms: 250
"#;
    let cfg = parse(yaml);
    assert_eq!(cfg.vendor, "acme");
    assert_eq!(cfg.priority, 5);
    assert_eq!(cfg.address, "http://vault.internal:8200");
    assert_eq!(cfg.token.as_ref().map(VaultToken::expose), Some("s.abc123"));
    assert_eq!(cfg.mount, "kv");
    assert_eq!(cfg.path_prefix, "cred");
    assert_eq!(cfg.namespace.as_deref(), Some("team-a"));
    assert_eq!(cfg.timeout_secs, 30);
    assert_eq!(cfg.retry.max_attempts, 5);
    assert_eq!(cfg.retry.base_delay_ms, 250);
    cfg.validate().expect("valid");
}

#[test]
fn config_rejects_unknown_fields() {
    let parsed: Result<VaultCredStorePluginConfig, _> =
        serde_saphyr::from_str("vendor: \"openbao\"\nunexpected: true\n");
    assert!(parsed.is_err());
}

#[test]
fn retry_block_rejects_unknown_fields() {
    let parsed: Result<VaultCredStorePluginConfig, _> =
        serde_saphyr::from_str("retry:\n  max_attempts: 2\n  jitter: true\n");
    assert!(parsed.is_err());
}

#[test]
fn retry_block_may_set_a_single_field() {
    let cfg = parse("retry:\n  max_attempts: 1\n");
    assert_eq!(cfg.retry.max_attempts, 1);
    assert_eq!(cfg.retry.base_delay_ms, DEFAULT_RETRY_BASE_DELAY_MS);
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
            let mut cfg = parse(r#"token: "${VAULT_CREDSTORE_PLUGIN_TEST_TOKEN}""#);
            cfg.expand_vars().expect("expand_vars should resolve");
            assert_eq!(
                cfg.token.as_ref().map(VaultToken::expose),
                Some("s.expanded-token")
            );
        },
    );
}

#[test]
fn expand_vars_expands_token_file_placeholder() {
    use toolkit::var_expand::ExpandVars;
    temp_env::with_var(
        "VAULT_CREDSTORE_PLUGIN_TEST_DIR",
        Some("/vault/secrets"),
        || {
            let mut cfg = parse(r#"token_file: "${VAULT_CREDSTORE_PLUGIN_TEST_DIR}/token""#);
            cfg.expand_vars().expect("expand_vars should resolve");
            assert_eq!(cfg.token_file.as_deref(), Some("/vault/secrets/token"));
        },
    );
}

#[test]
fn token_env_is_a_name_and_is_not_expanded() {
    use toolkit::var_expand::ExpandVars;
    let mut cfg = parse(r#"token_env: "VAULT_TOKEN""#);
    cfg.expand_vars().expect("nothing to expand");
    assert_eq!(cfg.token_env.as_deref(), Some("VAULT_TOKEN"));
}

#[test]
fn debug_does_not_leak_token() {
    let cfg = parse(r#"token: "s.super-secret-token""#);
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains("s.super-secret-token"));
    assert!(dbg.contains("<redacted>"));
}

// -- validation: token source ------------------------------------------------

#[test]
fn each_single_token_source_is_valid() {
    valid().validate().expect("inline token");

    let file = VaultCredStorePluginConfig {
        token_file: Some("/vault/secrets/token".to_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    file.validate().expect("token_file");

    let env = VaultCredStorePluginConfig {
        token_env: Some("VAULT_TOKEN".to_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    env.validate().expect("token_env");
}

#[test]
fn no_token_source_is_rejected() {
    let msg = rejection(&VaultCredStorePluginConfig::default());
    assert!(msg.contains("exactly one of"), "{msg}");
    assert!(msg.contains("none"), "{msg}");
}

#[test]
fn more_than_one_token_source_is_rejected() {
    let both = VaultCredStorePluginConfig {
        token_file: Some("/t".to_owned()),
        ..valid()
    };
    let msg = rejection(&both);
    assert!(msg.contains("exactly one of"), "{msg}");
    assert!(msg.contains("token, token_file"), "{msg}");

    let all = VaultCredStorePluginConfig {
        token_file: Some("/t".to_owned()),
        token_env: Some("X".to_owned()),
        ..valid()
    };
    assert!(rejection(&all).contains("token, token_file, token_env"));
}

#[test]
fn empty_token_sources_are_rejected() {
    let token = VaultCredStorePluginConfig {
        token: Some(VaultToken::from("  ")),
        ..VaultCredStorePluginConfig::default()
    };
    assert!(rejection(&token).contains("`token` is empty"));

    let file = VaultCredStorePluginConfig {
        token_file: Some(String::new()),
        ..VaultCredStorePluginConfig::default()
    };
    assert!(rejection(&file).contains("`token_file` is empty"));

    let env = VaultCredStorePluginConfig {
        token_env: Some(" ".to_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    assert!(rejection(&env).contains("`token_env` is empty"));
}

#[test]
fn validation_errors_never_contain_the_token() {
    let cfg = VaultCredStorePluginConfig {
        token: Some(VaultToken::from("s.super-secret-token")),
        token_file: Some("/t".to_owned()),
        mount: "/bad/".to_owned(),
        ..VaultCredStorePluginConfig::default()
    };
    assert!(!rejection(&cfg).contains("s.super-secret-token"));
}

// -- validation: addressing, timeout, retry ------------------------------------

#[test]
fn address_must_be_an_http_url() {
    for bad in ["", "  ", "vault.internal:8200", "ftp://vault"] {
        let cfg = VaultCredStorePluginConfig {
            address: bad.to_owned(),
            ..valid()
        };
        assert!(rejection(&cfg).contains("`address`"), "address {bad:?}");
    }
    for good in ["http://127.0.0.1:8200", "https://vault.internal"] {
        let cfg = VaultCredStorePluginConfig {
            address: good.to_owned(),
            ..valid()
        };
        cfg.validate().expect(good);
    }
}

#[test]
fn mount_and_prefix_must_be_clean_paths() {
    for bad in [
        "", "/secret", "secret/", "a//b", "a b", "a?b", "a#b", "a\nb",
    ] {
        let mount = VaultCredStorePluginConfig {
            mount: bad.to_owned(),
            ..valid()
        };
        assert!(rejection(&mount).contains("`mount`"), "mount {bad:?}");

        let prefix = VaultCredStorePluginConfig {
            path_prefix: bad.to_owned(),
            ..valid()
        };
        assert!(
            rejection(&prefix).contains("`path_prefix`"),
            "path_prefix {bad:?}"
        );
    }
    let nested = VaultCredStorePluginConfig {
        mount: "team/kv".to_owned(),
        path_prefix: "apps/credstore".to_owned(),
        ..valid()
    };
    nested.validate().expect("nested paths are fine");
}

#[test]
fn blank_namespace_is_rejected() {
    let cfg = VaultCredStorePluginConfig {
        namespace: Some("  ".to_owned()),
        ..valid()
    };
    assert!(rejection(&cfg).contains("`namespace`"));
}

#[test]
fn zero_timeout_is_rejected() {
    let cfg = VaultCredStorePluginConfig {
        timeout_secs: 0,
        ..valid()
    };
    assert!(rejection(&cfg).contains("`timeout_secs`"));
}

#[test]
fn retry_bounds_are_enforced() {
    let with = |max_attempts: u32, base_delay_ms: u64| VaultCredStorePluginConfig {
        retry: RetryConfig {
            max_attempts,
            base_delay_ms,
        },
        ..valid()
    };
    with(1, 1).validate().expect("lower bounds");
    with(10, 2000).validate().expect("upper bounds");

    assert!(rejection(&with(0, 100)).contains("`retry.max_attempts`"));
    assert!(rejection(&with(11, 100)).contains("`retry.max_attempts`"));
    assert!(rejection(&with(3, 0)).contains("`retry.base_delay_ms`"));
    assert!(rejection(&with(3, 2001)).contains("`retry.base_delay_ms`"));
}
