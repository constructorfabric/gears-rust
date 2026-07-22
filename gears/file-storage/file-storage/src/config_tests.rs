use super::*;

#[test]
fn default_max_url_ttl_is_seven_days() {
    let cfg = FileStorageConfig::default();
    assert_eq!(cfg.max_url_ttl_secs, 7 * 24 * 60 * 60);
}

#[test]
fn default_url_ttl_is_short_and_within_ceiling() {
    let cfg = FileStorageConfig::default();
    assert_eq!(cfg.default_url_ttl_secs, 15 * 60);
    assert!(
        cfg.default_url_ttl_secs <= cfg.max_url_ttl_secs,
        "default issuance TTL must not exceed the hard ceiling"
    );
}

#[test]
fn default_url_ttl_can_be_overridden() {
    let cfg: FileStorageConfig = serde_json::from_str(r#"{"default_url_ttl_secs": 300}"#).unwrap();
    assert_eq!(cfg.default_url_ttl_secs, 300);
}

#[test]
fn serde_default_applies_when_field_absent() {
    let cfg: FileStorageConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(
        cfg.max_url_ttl_secs,
        FileStorageConfig::default().max_url_ttl_secs,
        "serde(default) must fall back to the Default impl"
    );
}

#[test]
fn max_url_ttl_can_be_overridden() {
    let cfg: FileStorageConfig = serde_json::from_str(r#"{"max_url_ttl_secs": 3600}"#).unwrap();
    assert_eq!(cfg.max_url_ttl_secs, 3600);
}

#[test]
fn rejects_unknown_fields() {
    // deny_unknown_fields guards against silently-ignored config typos.
    let json = r#"{"max_url_ttl_secs": 60, "unexpected": true}"#;
    assert!(
        serde_json::from_str::<FileStorageConfig>(json).is_err(),
        "unknown keys must be rejected"
    );
}

#[test]
fn validate_rejects_zero_sweep_interval_when_sweep_enabled() {
    // A zero interval with the sweep on would spin the background loop tight.
    let cfg = FileStorageConfig {
        // Isolate this test to the sweep-interval check, not the (unrelated)
        // signing-key-seed guard added later.
        require_signing_key_seed: false,
        sweep_interval_secs: 0,
        enable_background_sweep: true,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "sweep_interval_secs == 0 must be rejected when the sweep is enabled"
    );
}

#[test]
fn validate_accepts_positive_sweep_interval_when_sweep_enabled() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        sweep_interval_secs: 60,
        enable_background_sweep: true,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "a positive sweep interval must pass validation"
    );
}

#[test]
fn validate_ignores_zero_sweep_interval_when_sweep_disabled() {
    // With the sweep off the interval is unused, so it need not be constrained.
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        sweep_interval_secs: 0,
        enable_background_sweep: false,
        ..FileStorageConfig::default()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn validate_rejects_missing_signing_key_seed_when_required_flag_set() {
    let cfg = FileStorageConfig {
        signing_key_seed: None,
        require_signing_key_seed: true,
        ..FileStorageConfig::default()
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
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "a missing signing_key_seed must be allowed when require_signing_key_seed is false"
    );
}

#[test]
fn validate_allows_present_signing_key_seed_when_required_flag_set() {
    let cfg = FileStorageConfig {
        signing_key_seed: Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned()),
        require_signing_key_seed: true,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "a present signing_key_seed must pass validation even when required"
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
fn require_finalize_internal_secret_without_secret_fails_validate() {
    // Mirrors `validate_rejects_missing_signing_key_seed_when_required_flag_set`:
    // a missing shared secret with the require flag set must fail fast
    // instead of silently downgrading to the token-only trust model for the
    // finalize/report-part s2s callbacks (P2 0.1 remaining).
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        finalize_internal_secret: None,
        require_finalize_internal_secret: true,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "a missing finalize_internal_secret must be rejected when \
         require_finalize_internal_secret is true"
    );
}

#[test]
fn require_finalize_internal_secret_with_secret_passes_validate() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        finalize_internal_secret: Some("interim-shared-secret".to_owned()),
        require_finalize_internal_secret: true,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "a present finalize_internal_secret must pass validation even when required"
    );
}

#[test]
fn default_require_finalize_internal_secret_is_false() {
    assert!(
        !FileStorageConfig::default().require_finalize_internal_secret,
        "require_finalize_internal_secret must default to false so existing deployments \
         and not-yet-redeployed sidecars keep working"
    );
}

#[test]
fn default_enable_background_sweep_is_true() {
    assert!(
        FileStorageConfig::default().enable_background_sweep,
        "enable_background_sweep must default to true; deployments that need \
         deterministic behavior (tests, some e2e profiles) must opt out explicitly"
    );
}

#[test]
fn serde_round_trip_preserves_value() {
    let original = FileStorageConfig {
        max_url_ttl_secs: 12_345,
        ..FileStorageConfig::default()
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: FileStorageConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.max_url_ttl_secs, original.max_url_ttl_secs);
}

#[test]
fn config_s3_backends_serde_round_trip() {
    // P2 1.7.3 config wiring: `s3_backends` must serde round-trip faithfully,
    // and `secret_access_key` must never leak through `FileStorageConfig`'s
    // (or `S3BackendConfig`'s own) `Debug` output.
    const SECRET: &str = "super-secret-value-do-not-print-me";

    let original = FileStorageConfig {
        s3_backends: vec![S3BackendConfig {
            id: "s3-primary".to_owned(),
            endpoint: Some("http://127.0.0.1:9000".to_owned()),
            region: "us-east-1".to_owned(),
            bucket: "my-bucket".to_owned(),
            access_key_id: Some("AKIAEXAMPLE".to_owned()),
            secret_access_key: Some(SECRET.to_owned()),
            path_style: true,
        }],
        ..FileStorageConfig::default()
    };

    let json = serde_json::to_string(&original).unwrap();
    let back: FileStorageConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(back.s3_backends.len(), 1);
    let entry = &back.s3_backends[0];
    assert_eq!(entry.id, "s3-primary");
    assert_eq!(entry.endpoint.as_deref(), Some("http://127.0.0.1:9000"));
    assert_eq!(entry.region, "us-east-1");
    assert_eq!(entry.bucket, "my-bucket");
    assert_eq!(entry.access_key_id.as_deref(), Some("AKIAEXAMPLE"));
    assert_eq!(entry.secret_access_key.as_deref(), Some(SECRET));
    assert!(entry.path_style);

    // Redaction proof: the raw secret must not appear anywhere in either
    // struct's `Debug` output.
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
fn config_s3_backends_defaults_to_empty() {
    let cfg: FileStorageConfig = serde_json::from_str("{}").unwrap();
    assert!(
        cfg.s3_backends.is_empty(),
        "s3_backends must default to empty so existing configs keep parsing"
    );
}

#[test]
fn config_default_backend_id_defaults_to_none() {
    // P2 1.7 Stage 6: an existing config without `default_backend_id` must
    // keep parsing and keep `local-fs` as the implicit default (enforced by
    // `gear.rs::build_backend_registry` falling back to `LOCAL_FS_ID`).
    let cfg: FileStorageConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(cfg.default_backend_id, None);
}

#[test]
fn config_default_backend_id_serde_round_trip() {
    let original = FileStorageConfig {
        default_backend_id: Some("s3-primary".to_owned()),
        ..FileStorageConfig::default()
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: FileStorageConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.default_backend_id.as_deref(), Some("s3-primary"));
}

// ── url-ttl / orphan-grace cross-field validation ───────────────────────────
//
// The live-multipart-session guard (retention-cleanup.md §"Live-Multipart-
// Session Guard") reasons that a long-running upload can legitimately keep
// its backing pending version alive past `orphan_grace_secs`, for as long as
// the signed URL driving it stays valid. That reasoning applies verbatim to
// single-part PUT URLs, which have no session row to guard on -- so a
// deployment raising `default_url_ttl_secs`/`max_url_ttl_secs` above
// `orphan_grace_secs` risks the orphan-reconciliation sweep deleting a
// still-pending version out from under a still-valid in-flight PUT.

#[test]
fn validate_rejects_default_url_ttl_exceeding_orphan_grace() {
    let cfg = FileStorageConfig {
        default_url_ttl_secs: 7200,
        require_signing_key_seed: false,
        orphan_grace_secs: 3600,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "default_url_ttl_secs exceeding orphan_grace_secs is a direct self-contradiction \
         and must be rejected"
    );
}

#[test]
fn validate_accepts_default_url_ttl_equal_to_orphan_grace() {
    // The boundary itself (`==`, not `>`) must not be rejected.
    let cfg = FileStorageConfig {
        default_url_ttl_secs: 3600,
        require_signing_key_seed: false,
        orphan_grace_secs: 3600,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "default_url_ttl_secs == orphan_grace_secs must be accepted"
    );
}

// ── multipart-session-ttl / default-url-ttl cross-field validation ─────────
//
// The multipart session's own lifetime (`multipart_session_ttl_secs`) must
// be decoupled from the short per-part signed-URL TTL (`default_url_ttl_secs`)
// -- see `MultipartService::session_ttl_secs`'s doc comment -- but it must
// never be *shorter* than that URL TTL, or the very first batch of per-part
// URLs minted at initiate time could outlive the session itself.

#[test]
fn default_multipart_session_ttl_is_24_hours() {
    let cfg = FileStorageConfig::default();
    assert_eq!(cfg.multipart_session_ttl_secs, 86400);
}

#[test]
fn multipart_session_ttl_can_be_overridden() {
    let cfg: FileStorageConfig =
        serde_json::from_str(r#"{"multipart_session_ttl_secs": 3600}"#).unwrap();
    assert_eq!(cfg.multipart_session_ttl_secs, 3600);
}

#[test]
fn validate_rejects_multipart_session_ttl_shorter_than_default_url_ttl() {
    let cfg = FileStorageConfig {
        default_url_ttl_secs: 900,
        multipart_session_ttl_secs: 300,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "multipart_session_ttl_secs shorter than default_url_ttl_secs is a direct \
         self-contradiction and must be rejected"
    );
}

#[test]
fn validate_accepts_multipart_session_ttl_equal_to_default_url_ttl() {
    let cfg = FileStorageConfig {
        default_url_ttl_secs: 900,
        multipart_session_ttl_secs: 900,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "multipart_session_ttl_secs == default_url_ttl_secs must be accepted"
    );
}

#[test]
fn validate_accepts_default_config_multipart_session_ttl() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.multipart_session_ttl_secs > cfg.default_url_ttl_secs,
        "sanity: the stock defaults are 24h session vs 15min url ttl"
    );
    assert!(cfg.validate().is_ok());
}

#[test]
fn validate_accepts_default_config_despite_max_url_ttl_exceeding_orphan_grace() {
    // The stock defaults are `max_url_ttl_secs = 7 days` and
    // `orphan_grace_secs = 1 hour` -- `max_url_ttl_secs` alone exceeding
    // `orphan_grace_secs` must stay a warning (see `FileStorageConfig::validate`),
    // never a hard failure, or every default deployment would refuse to boot.
    // `require_signing_key_seed` is turned off, same as every other test in
    // this file that isn't specifically exercising that (unrelated) guard.
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.max_url_ttl_secs > cfg.orphan_grace_secs,
        "sanity: the defaults must exhibit the condition this test exercises"
    );
    assert!(
        cfg.validate().is_ok(),
        "the stock default config (module-config knobs only) must pass validation"
    );
}

// ── default-url-ttl / max-url-ttl cross-field validation ───────────────────
//
// `default_url_ttl_secs` is what every mint uses absent a caller override, so
// it must itself respect `max_url_ttl_secs` -- otherwise the very first URL
// minted with no override would already violate the ceiling the control
// plane is supposed to enforce.

#[test]
fn validate_rejects_default_url_ttl_exceeding_max_url_ttl() {
    let cfg = FileStorageConfig {
        default_url_ttl_secs: 200,
        max_url_ttl_secs: 100,
        require_signing_key_seed: false,
        orphan_grace_secs: 200,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "default_url_ttl_secs exceeding max_url_ttl_secs is a direct self-contradiction \
         and must be rejected"
    );
}

#[test]
fn validate_accepts_default_url_ttl_equal_to_max_url_ttl() {
    let cfg = FileStorageConfig {
        default_url_ttl_secs: 100,
        max_url_ttl_secs: 100,
        multipart_session_ttl_secs: 100,
        require_signing_key_seed: false,
        orphan_grace_secs: 100,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "default_url_ttl_secs == max_url_ttl_secs must be accepted"
    );
}

#[test]
fn validate_accepts_default_config_url_ttl_pair() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.default_url_ttl_secs <= cfg.max_url_ttl_secs,
        "sanity: the stock defaults must not exhibit the condition this test guards against"
    );
    assert!(cfg.validate().is_ok());
}

// ── default-page-size / max-page-size cross-field validation ───────────────
//
// `default_page_size` is what `GET /files` uses absent a caller-supplied
// `limit`, so it must not itself exceed the cap `max_page_size` claims to
// enforce on every page.

#[test]
fn validate_rejects_default_page_size_exceeding_max_page_size() {
    let cfg = FileStorageConfig {
        default_page_size: 2000,
        max_page_size: 1000,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "default_page_size exceeding max_page_size is a direct self-contradiction \
         and must be rejected"
    );
}

#[test]
fn validate_accepts_default_page_size_equal_to_max_page_size() {
    let cfg = FileStorageConfig {
        default_page_size: 500,
        max_page_size: 500,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "default_page_size == max_page_size must be accepted"
    );
}

#[test]
fn validate_accepts_default_config_page_size_pair() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.default_page_size <= cfg.max_page_size,
        "sanity: the stock defaults must not exhibit the condition this test guards against"
    );
    assert!(cfg.validate().is_ok());
}
