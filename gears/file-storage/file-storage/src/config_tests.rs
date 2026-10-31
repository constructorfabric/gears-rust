use super::*;

/// Config with the mandatory internal secret set, so a test isolates the check it targets.
fn cfg_with_secret() -> FileStorageConfig {
    FileStorageConfig {
        finalize_internal_secret: Some(SecretString::new("test-internal-secret")),
        ..FileStorageConfig::default()
    }
}

#[test]
fn empty_config_parses_and_default_config_validates() {
    let empty: FileStorageConfig = serde_json::from_str("{}").unwrap();
    assert!(empty.s3_backends.is_empty());
    let cfg = FileStorageConfig {
        signing_key_seed: Some(SecretString::new("seed")),
        ..cfg_with_secret()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn unknown_and_removed_keys_are_rejected() {
    for key in [
        "unexpected",
        "enable_background_sweep",
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
fn validate_signing_key_seed_requirement() {
    for (seed, required, accepted) in [
        (None, true, false),
        (None, false, true),
        (Some("seed"), true, true),
    ] {
        let cfg = FileStorageConfig {
            signing_key_seed: seed.map(SecretString::new),
            require_signing_key_seed: required,
            ..cfg_with_secret()
        };
        assert_eq!(
            cfg.validate().is_ok(),
            accepted,
            "seed={seed:?} required={required}"
        );
    }
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
fn debug_output_redacts_every_secret() {
    let cfg = FileStorageConfig {
        signing_key_seed: Some(SecretString::new("seed-secret")),
        s3_backends: vec![S3BackendConfig {
            id: "s3-primary".to_owned(),
            endpoint: None,
            region: "us-east-1".to_owned(),
            bucket: "my-bucket".to_owned(),
            access_key_id: None,
            secret_access_key: Some(SecretString::new("s3-secret")),
            path_style: true,
        }],
        finalize_internal_secret: Some(SecretString::new("internal-secret")),
        ..FileStorageConfig::default()
    };
    let debug = format!("{cfg:?}");
    for secret in ["seed-secret", "internal-secret", "s3-secret"] {
        assert!(!debug.contains(secret), "Debug leaks {secret}: {debug}");
    }
    assert!(debug.contains("<redacted>"), "{debug}");
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
