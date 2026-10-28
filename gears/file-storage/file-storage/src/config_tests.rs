use super::*;

#[test]
fn defaults_apply_to_an_empty_config_and_can_be_overridden() {
    let default = FileStorageConfig::default();
    let empty: FileStorageConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(
        empty.max_url_ttl_secs, default.max_url_ttl_secs,
        "serde(default) must fall back to the Default impl"
    );
    assert_eq!(default.max_url_ttl_secs, 7 * 24 * 60 * 60);
    assert_eq!(default.default_url_ttl_secs, 15 * 60);
    assert!(
        default.default_url_ttl_secs <= default.max_url_ttl_secs,
        "default issuance TTL must not exceed the hard ceiling"
    );
    assert!(
        empty.s3_backends.is_empty(),
        "s3_backends must default to empty so existing configs keep parsing"
    );
    assert_eq!(empty.default_backend_id, None);

    let overridden: FileStorageConfig =
        serde_json::from_str(r#"{"default_url_ttl_secs": 300, "max_url_ttl_secs": 3600}"#).unwrap();
    assert_eq!(overridden.default_url_ttl_secs, 300);
    assert_eq!(overridden.max_url_ttl_secs, 3600);
}

#[test]
fn rejects_unknown_fields() {
    let json = r#"{"max_url_ttl_secs": 60, "unexpected": true}"#;
    assert!(
        serde_json::from_str::<FileStorageConfig>(json).is_err(),
        "unknown keys must be rejected"
    );
}

/// Config with the mandatory internal secret set, so a test isolates the check it targets.
fn cfg_with_secret() -> FileStorageConfig {
    FileStorageConfig {
        finalize_internal_secret: Some(SecretString::new("test-internal-secret")),
        ..FileStorageConfig::default()
    }
}

#[test]
fn removed_background_sweep_keys_are_rejected() {
    // The background sweep keys were removed; deny_unknown_fields rejects them.
    for key in [
        "enable_background_sweep",
        "sweep_interval_secs",
        "orphan_grace_secs",
        "require_finalize_internal_secret",
    ] {
        let json = format!(r#"{{"{key}": 1}}"#);
        assert!(
            serde_json::from_str::<FileStorageConfig>(&json).is_err(),
            "{key} must be rejected as an unknown field"
        );
    }
}

#[test]
fn validate_rejects_missing_signing_key_seed_when_required_flag_set() {
    let cfg = FileStorageConfig {
        signing_key_seed: None,
        require_signing_key_seed: true,
        ..cfg_with_secret()
    };
    assert!(
        cfg.validate().is_err(),
        "a missing signing_key_seed must be rejected when require_signing_key_seed is true"
    );
}

#[test]
fn validate_allows_missing_signing_key_seed_when_required_flag_unset() {
    let cfg = FileStorageConfig {
        signing_key_seed: None,
        require_signing_key_seed: false,
        ..cfg_with_secret()
    };
    assert!(
        cfg.validate().is_ok(),
        "a missing signing_key_seed must be allowed when require_signing_key_seed is false"
    );
}

#[test]
fn validate_allows_present_signing_key_seed_when_required_flag_set() {
    const SEED: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let cfg = FileStorageConfig {
        signing_key_seed: Some(SecretString::new(SEED)),
        require_signing_key_seed: true,
        ..cfg_with_secret()
    };
    assert!(
        cfg.validate().is_ok(),
        "a present signing_key_seed must pass validation even when required"
    );

    let cfg_debug = format!("{cfg:?}");
    assert!(
        !cfg_debug.contains(SEED),
        "FileStorageConfig's Debug output must never contain the raw signing_key_seed: {cfg_debug}"
    );
}

#[test]
fn default_require_signing_key_seed_is_true() {
    assert!(
        FileStorageConfig::default().require_signing_key_seed,
        "require_signing_key_seed must default to true (secure-by-default)"
    );
}

#[test]
fn missing_finalize_internal_secret_fails_validate() {
    // The s2s finalize/report-part callbacks require the internal credential;
    // finalize trusts the size/hash the sidecar reports on them.
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        finalize_internal_secret: None,
        ..FileStorageConfig::default()
    };
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("finalize_internal_secret"), "{err}");
    assert!(err.contains("FS_SIDECAR_INTERNAL_TOKEN"), "{err}");

    let empty = FileStorageConfig {
        finalize_internal_secret: Some(SecretString::new("")),
        ..cfg
    };
    assert!(
        empty.validate().is_err(),
        "an empty secret must be rejected"
    );
}

#[test]
fn present_finalize_internal_secret_passes_validate_and_is_redacted() {
    const SECRET: &str = "interim-shared-secret";
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        finalize_internal_secret: Some(SecretString::new(SECRET)),
        ..FileStorageConfig::default()
    };
    assert!(cfg.validate().is_ok());

    let cfg_debug = format!("{cfg:?}");
    assert!(
        !cfg_debug.contains(SECRET),
        "FileStorageConfig's Debug output must never contain the raw finalize_internal_secret: {cfg_debug}"
    );
}

#[test]
fn config_serde_round_trip() {
    // Values round-trip through serde; `secret_access_key` never leaks via `Debug`.
    const SECRET: &str = "super-secret-value-do-not-print-me";

    let original = FileStorageConfig {
        max_url_ttl_secs: 12_345,
        s3_backends: vec![S3BackendConfig {
            id: "s3-primary".to_owned(),
            endpoint: Some("http://127.0.0.1:9000".to_owned()),
            region: "us-east-1".to_owned(),
            bucket: "my-bucket".to_owned(),
            access_key_id: Some("AKIAEXAMPLE".to_owned()),
            secret_access_key: Some(SecretString::new(SECRET)),
            path_style: true,
        }],
        default_backend_id: Some("s3-primary".to_owned()),
        ..FileStorageConfig::default()
    };

    let json = serde_json::to_string(&original).unwrap();
    let back: FileStorageConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(back.max_url_ttl_secs, 12_345);
    assert_eq!(back.default_backend_id.as_deref(), Some("s3-primary"));
    assert_eq!(back.s3_backends.len(), 1);
    let entry = &back.s3_backends[0];
    assert_eq!(entry.id, "s3-primary");
    assert_eq!(entry.endpoint.as_deref(), Some("http://127.0.0.1:9000"));
    assert_eq!(entry.region, "us-east-1");
    assert_eq!(entry.bucket, "my-bucket");
    assert_eq!(entry.access_key_id.as_deref(), Some("AKIAEXAMPLE"));
    assert_eq!(
        entry
            .secret_access_key
            .as_ref()
            .map(toolkit_utils::SecretString::expose),
        Some(SECRET)
    );
    assert!(entry.path_style);

    let cfg_debug = format!("{back:?}");
    assert!(
        !cfg_debug.contains(SECRET),
        "FileStorageConfig's Debug output must never contain the raw secret_access_key: {cfg_debug}"
    );
    let entry_debug = format!("{entry:?}");
    assert!(
        !entry_debug.contains(SECRET),
        "S3BackendConfig's Debug output must never contain the raw secret_access_key: {entry_debug}"
    );
    assert!(cfg_debug.contains("<redacted>"));
}

#[test]
fn default_page_sizes_are_valid_and_within_ceiling() {
    let cfg = FileStorageConfig::default();
    assert_eq!(cfg.default_page_size, 25);
    assert_eq!(cfg.max_page_size, 200);
    assert!(
        cfg.max_page_size <= MAX_PAGE_SIZE_CEILING,
        "the shipped default_max_page_size must not itself exceed the ceiling"
    );
}

#[test]
fn validate_page_size_bounds() {
    let with_sizes = |default_page_size, max_page_size| FileStorageConfig {
        default_page_size,
        max_page_size,
        ..cfg_with_secret()
    };
    assert_validate_cases(vec![
        ("default above max", with_sizes(150, 100), false),
        ("default equal to max", with_sizes(100, 100), true),
        (
            "max at ceiling",
            with_sizes(MAX_PAGE_SIZE_CEILING, MAX_PAGE_SIZE_CEILING),
            true,
        ),
        (
            "max above ceiling",
            with_sizes(MAX_PAGE_SIZE_CEILING, MAX_PAGE_SIZE_CEILING + 1),
            false,
        ),
        ("zero default", with_sizes(0, 100), false),
        ("zero max", with_sizes(25, 0), false),
    ]);
}

/// Each case sets the key(s) under test on a config that is otherwise valid; `true` = `validate`
/// must accept it.
fn assert_validate_cases(cases: Vec<(&str, FileStorageConfig, bool)>) {
    for (name, cfg, accepted) in cases {
        let cfg = FileStorageConfig {
            require_signing_key_seed: false,
            ..cfg
        };
        assert_eq!(
            cfg.validate().is_ok(),
            accepted,
            "{name}: expected {}",
            if accepted { "accepted" } else { "rejected" }
        );
    }
}

#[test]
fn finalize_grace_session_ttl_and_complete_lease_defaults_and_overrides() {
    let cfg = FileStorageConfig::default();
    assert_eq!(cfg.finalize_token_grace_secs, 3600);
    assert_eq!(cfg.multipart_session_ttl_secs, 86400);
    assert!(
        cfg.multipart_session_ttl_secs > cfg.default_url_ttl_secs,
        "the stock session lifetime must outlive the per-part URL TTL"
    );
    assert!(cfg.multipart_session_ttl_secs <= MAX_MULTIPART_SESSION_TTL_SECS);
    assert_eq!(cfg.multipart_complete_lease_secs, 120);
    assert!(cfg.multipart_complete_lease_secs <= MAX_MULTIPART_COMPLETE_LEASE_SECS);

    let cfg: FileStorageConfig = serde_json::from_str(
        r#"{"finalize_token_grace_secs": 0, "multipart_session_ttl_secs": 3600,
            "multipart_complete_lease_secs": 30}"#,
    )
    .unwrap();
    assert_eq!(cfg.finalize_token_grace_secs, 0);
    assert_eq!(cfg.multipart_session_ttl_secs, 3600);
    assert_eq!(cfg.multipart_complete_lease_secs, 30);
}

// Guards the saturating `i64` conversion: an unbounded grace would become `i64::MAX` seconds.
#[test]
fn validate_finalize_token_grace_bounds() {
    let with_grace = |secs| FileStorageConfig {
        finalize_token_grace_secs: secs,
        ..cfg_with_secret()
    };
    assert_validate_cases(vec![
        (
            "grace at MAX",
            with_grace(MAX_FINALIZE_TOKEN_GRACE_SECS),
            true,
        ),
        (
            "grace above MAX",
            with_grace(MAX_FINALIZE_TOKEN_GRACE_SECS + 1),
            false,
        ),
        ("zero grace (disabled)", with_grace(0), true),
    ]);
}

#[test]
fn validate_multipart_session_ttl_bounds() {
    let with_ttl = |url_ttl, session_ttl| FileStorageConfig {
        default_url_ttl_secs: url_ttl,
        multipart_session_ttl_secs: session_ttl,
        ..cfg_with_secret()
    };
    let default_url_ttl = FileStorageConfig::default().default_url_ttl_secs;
    assert_validate_cases(vec![
        (
            "shorter than default_url_ttl_secs (self-contradiction)",
            with_ttl(900, 300),
            false,
        ),
        ("equal to default_url_ttl_secs", with_ttl(900, 900), true),
        (
            "at ceiling",
            with_ttl(default_url_ttl, MAX_MULTIPART_SESSION_TTL_SECS),
            true,
        ),
        (
            "above ceiling",
            with_ttl(default_url_ttl, MAX_MULTIPART_SESSION_TTL_SECS + 1),
            false,
        ),
        ("zero", with_ttl(default_url_ttl, 0), false),
        ("one", with_ttl(1, 1), true),
    ]);
}

// The lease must be bounded: an oversized value would delay another caller taking over.
#[test]
fn validate_multipart_complete_lease_bounds() {
    let with_lease = |secs| FileStorageConfig {
        multipart_complete_lease_secs: secs,
        ..cfg_with_secret()
    };
    assert_validate_cases(vec![
        (
            "at ceiling",
            with_lease(MAX_MULTIPART_COMPLETE_LEASE_SECS),
            true,
        ),
        (
            "above ceiling",
            with_lease(MAX_MULTIPART_COMPLETE_LEASE_SECS + 1),
            false,
        ),
        ("zero", with_lease(0), false),
        ("one", with_lease(1), true),
    ]);
}
