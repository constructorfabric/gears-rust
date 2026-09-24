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
fn default_finalize_token_grace_is_one_hour() {
    let cfg = FileStorageConfig::default();
    assert_eq!(cfg.finalize_token_grace_secs, 3600);
}

#[test]
fn finalize_token_grace_can_be_overridden() {
    let cfg: FileStorageConfig =
        serde_json::from_str(r#"{"finalize_token_grace_secs": 0}"#).unwrap();
    assert_eq!(cfg.finalize_token_grace_secs, 0);
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
    const SEED: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let cfg = FileStorageConfig {
        signing_key_seed: Some(SecretString::new(SEED)),
        require_signing_key_seed: true,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "a present signing_key_seed must pass validation even when required"
    );

    // Redaction proof: the raw seed must not appear in Debug output.
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
    const SECRET: &str = "interim-shared-secret";
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        finalize_internal_secret: Some(SecretString::new(SECRET)),
        require_finalize_internal_secret: true,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "a present finalize_internal_secret must pass validation even when required"
    );

    // Redaction proof: the raw secret must not appear in Debug output.
    let cfg_debug = format!("{cfg:?}");
    assert!(
        !cfg_debug.contains(SECRET),
        "FileStorageConfig's Debug output must never contain the raw finalize_internal_secret: {cfg_debug}"
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
            secret_access_key: Some(SecretString::new(SECRET)),
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
    assert_eq!(
        entry
            .secret_access_key
            .as_ref()
            .map(toolkit_utils::SecretString::expose),
        Some(SECRET)
    );
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

// ── default_url_ttl_secs lower bound ────────────────────────────────────────
//
// `MultipartService` applies `url_ttl_secs.max(1)` as defense-in-depth
// against a zero TTL reaching `checked_add`; unlike `finalize_token_grace_secs`,
// `0` has no documented "disabled" meaning here, so `validate()` must reject
// it outright rather than let every signed URL silently get a 1-second TTL.

#[test]
fn validate_rejects_zero_default_url_ttl() {
    let cfg = FileStorageConfig {
        default_url_ttl_secs: 0,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "default_url_ttl_secs == 0 must be rejected"
    );
}

#[test]
fn validate_accepts_default_url_ttl_of_one() {
    let cfg = FileStorageConfig {
        default_url_ttl_secs: 1,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "default_url_ttl_secs == 1 must be accepted"
    );
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

// ── max_page_size absolute ceiling ──────────────────────────────────────────
//
// `max_page_size` otherwise has no ceiling of its own: an operator could
// configure it arbitrarily large, and `MetadataRepo::list_for_files`/
// `VersionRepo::get_manifests`'s own chunking only prevents a hard driver
// failure -- it does not prevent every listing request from directly
// inflating its row count, chunk count, response size, and latency by
// whatever `max_page_size` is set to.

#[test]
fn validate_accepts_max_page_size_at_ceiling() {
    let cfg = FileStorageConfig {
        default_page_size: MAX_PAGE_SIZE_CEILING,
        max_page_size: MAX_PAGE_SIZE_CEILING,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "max_page_size == MAX_PAGE_SIZE_CEILING must be accepted"
    );
}

#[test]
fn validate_rejects_max_page_size_above_ceiling() {
    let cfg = FileStorageConfig {
        default_page_size: MAX_PAGE_SIZE_CEILING,
        max_page_size: MAX_PAGE_SIZE_CEILING + 1,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "max_page_size exceeding MAX_PAGE_SIZE_CEILING must be rejected regardless of what an \
         operator configures"
    );
}

#[test]
fn validate_accepts_default_config_max_page_size() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.max_page_size <= MAX_PAGE_SIZE_CEILING,
        "sanity: the shipped default_max_page_size must not itself exceed the ceiling"
    );
    assert!(cfg.validate().is_ok());
}

// ── finalize_token_grace_secs upper bound ───────────────────────────────────
//
// `gear.rs` converts `finalize_token_grace_secs` to `i64` via a saturating
// `unwrap_or(i64::MAX)`; without a ceiling here an oversized value would
// silently become `i64::MAX` seconds of grace, making the s2s finalize/
// report-part callbacks' `exp` check a de-facto no-op.

#[test]
fn validate_accepts_finalize_token_grace_at_max() {
    let cfg = FileStorageConfig {
        finalize_token_grace_secs: MAX_FINALIZE_TOKEN_GRACE_SECS,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "finalize_token_grace_secs == MAX_FINALIZE_TOKEN_GRACE_SECS must be accepted"
    );
}

#[test]
fn validate_rejects_finalize_token_grace_above_max() {
    let cfg = FileStorageConfig {
        finalize_token_grace_secs: MAX_FINALIZE_TOKEN_GRACE_SECS + 1,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "finalize_token_grace_secs exceeding MAX_FINALIZE_TOKEN_GRACE_SECS must be rejected"
    );
}

#[test]
fn validate_accepts_zero_finalize_token_grace() {
    let cfg = FileStorageConfig {
        finalize_token_grace_secs: 0,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "finalize_token_grace_secs == 0 (grace disabled) must be accepted"
    );
}

// ── max_url_ttl_secs absolute ceiling ───────────────────────────────────────
//
// `gear.rs` converts `max_url_ttl_secs` to `i64` via the same saturating
// `unwrap_or(i64::MAX)` pattern as `finalize_token_grace_secs`, and
// `Issuer::issue` then adds it directly to `now.unix_timestamp()` to compute
// `max_exp`; without a ceiling here an oversized value would overflow that
// addition instead of just clamping token lifetime as intended.

#[test]
fn validate_accepts_max_url_ttl_at_ceiling() {
    let cfg = FileStorageConfig {
        max_url_ttl_secs: MAX_URL_TTL_CEILING,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "max_url_ttl_secs == MAX_URL_TTL_CEILING must be accepted"
    );
}

#[test]
fn validate_rejects_max_url_ttl_above_ceiling() {
    let cfg = FileStorageConfig {
        max_url_ttl_secs: MAX_URL_TTL_CEILING + 1,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "max_url_ttl_secs exceeding MAX_URL_TTL_CEILING must be rejected"
    );
}

#[test]
fn validate_accepts_default_config_max_url_ttl() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.max_url_ttl_secs <= MAX_URL_TTL_CEILING,
        "sanity: the shipped default_max_url_ttl_secs must not itself exceed the ceiling"
    );
    assert!(cfg.validate().is_ok());
}

// ── multipart_session_ttl_secs absolute ceiling ─────────────────────────────
//
// `gear.rs` converts `multipart_session_ttl_secs` to `i64` via the same
// saturating `unwrap_or(i64::MAX)` pattern as `finalize_token_grace_secs`,
// and `MultipartService::initiate_multipart_upload` then adds it directly to
// `now` to compute the session's `expires_at`; without a ceiling here an
// oversized (or corrupted/malicious) config value would overflow that
// addition instead of just producing a long-lived session as intended.

#[test]
fn validate_accepts_multipart_session_ttl_at_ceiling() {
    let cfg = FileStorageConfig {
        multipart_session_ttl_secs: MAX_MULTIPART_SESSION_TTL_SECS,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "multipart_session_ttl_secs == MAX_MULTIPART_SESSION_TTL_SECS must be accepted"
    );
}

#[test]
fn validate_rejects_multipart_session_ttl_above_ceiling() {
    let cfg = FileStorageConfig {
        multipart_session_ttl_secs: MAX_MULTIPART_SESSION_TTL_SECS + 1,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "multipart_session_ttl_secs exceeding MAX_MULTIPART_SESSION_TTL_SECS must be rejected"
    );
}

#[test]
fn validate_accepts_default_config_multipart_session_ttl_ceiling() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.multipart_session_ttl_secs <= MAX_MULTIPART_SESSION_TTL_SECS,
        "sanity: the shipped default_multipart_session_ttl_secs must not itself exceed the \
         ceiling"
    );
    assert!(cfg.validate().is_ok());
}

// ── multipart_session_ttl_secs lower bound ──────────────────────────────────
//
// `MultipartService` applies `session_ttl_secs.max(1)` as defense-in-depth
// against a zero TTL reaching `checked_add`; `0` has no documented "disabled"
// meaning for a session lifetime, so `validate()` must reject it outright.

#[test]
fn validate_rejects_zero_multipart_session_ttl() {
    let cfg = FileStorageConfig {
        multipart_session_ttl_secs: 0,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "multipart_session_ttl_secs == 0 must be rejected"
    );
}

#[test]
fn validate_accepts_multipart_session_ttl_of_one() {
    let cfg = FileStorageConfig {
        default_url_ttl_secs: 1,
        multipart_session_ttl_secs: 1,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "multipart_session_ttl_secs == 1 must be accepted"
    );
}

// ── multipart_complete_lease_secs absolute ceiling ──────────────────────────
//
// Unlike the TTL/grace knobs above, `gear.rs` already falls back to a safe
// finite default on conversion (`unwrap_or(120)`, not `i64::MAX`), so this
// isn't the same silent-saturation overflow hazard -- but the lease bounds
// how long one `complete` call may hold the `completing` state before
// another caller can take it over after a crash, and nothing otherwise stops
// an operator from configuring a value that defeats that purpose.

#[test]
fn validate_accepts_multipart_complete_lease_at_ceiling() {
    let cfg = FileStorageConfig {
        multipart_complete_lease_secs: MAX_MULTIPART_COMPLETE_LEASE_SECS,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "multipart_complete_lease_secs == MAX_MULTIPART_COMPLETE_LEASE_SECS must be accepted"
    );
}

#[test]
fn validate_rejects_multipart_complete_lease_above_ceiling() {
    let cfg = FileStorageConfig {
        multipart_complete_lease_secs: MAX_MULTIPART_COMPLETE_LEASE_SECS + 1,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "multipart_complete_lease_secs exceeding MAX_MULTIPART_COMPLETE_LEASE_SECS must be \
         rejected"
    );
}

#[test]
fn validate_accepts_default_config_multipart_complete_lease() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.multipart_complete_lease_secs <= MAX_MULTIPART_COMPLETE_LEASE_SECS,
        "sanity: the shipped default_multipart_complete_lease_secs must not itself exceed the \
         ceiling"
    );
    assert!(cfg.validate().is_ok());
}

// ── multipart_complete_lease_secs lower bound ───────────────────────────────
//
// `MultipartService` applies `complete_lease_secs.max(1)` as defense-in-depth
// against a zero TTL reaching `checked_add`; `0` has no documented "disabled"
// meaning for the lease, so `validate()` must reject it outright rather than
// let another caller take the lease over almost immediately.

#[test]
fn validate_rejects_zero_multipart_complete_lease() {
    let cfg = FileStorageConfig {
        multipart_complete_lease_secs: 0,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "multipart_complete_lease_secs == 0 must be rejected"
    );
}

#[test]
fn validate_accepts_multipart_complete_lease_of_one() {
    let cfg = FileStorageConfig {
        multipart_complete_lease_secs: 1,
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "multipart_complete_lease_secs == 1 must be accepted"
    );
}

// ── orphan_grace_secs absolute ceiling ──────────────────────────────────────
//
// `domain::cleanup::CleanupEngine::run_sweep` subtracts it from `now` (via an
// already-safe finite fallback, `unwrap_or(3600)`, not `i64::MAX`), so this
// isn't an overflow hazard the way the addition sites above are -- but it
// otherwise has no ceiling of its own.

#[test]
fn validate_accepts_orphan_grace_at_ceiling() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        orphan_grace_secs: MAX_ORPHAN_GRACE_SECS,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "orphan_grace_secs == MAX_ORPHAN_GRACE_SECS must be accepted"
    );
}

#[test]
fn validate_rejects_orphan_grace_above_ceiling() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        orphan_grace_secs: MAX_ORPHAN_GRACE_SECS + 1,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "orphan_grace_secs exceeding MAX_ORPHAN_GRACE_SECS must be rejected"
    );
}

#[test]
fn validate_accepts_default_config_orphan_grace_ceiling() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.orphan_grace_secs <= MAX_ORPHAN_GRACE_SECS,
        "sanity: the shipped default_orphan_grace_secs must not itself exceed the ceiling"
    );
    assert!(cfg.validate().is_ok());
}

// ── idempotency_ttl_secs absolute ceiling ───────────────────────────────────
//
// `FileService::create_file` adds it directly to `now` to compute the stored
// idempotency record's `expires_at` (via an already-safe finite fallback,
// `unwrap_or(86400)`, not `i64::MAX`), so this isn't an overflow hazard the
// way the `i64::MAX`-fallback sites above are -- but it otherwise has no
// ceiling of its own.

#[test]
fn validate_accepts_idempotency_ttl_at_ceiling() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        idempotency_ttl_secs: MAX_IDEMPOTENCY_TTL_SECS,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "idempotency_ttl_secs == MAX_IDEMPOTENCY_TTL_SECS must be accepted"
    );
}

#[test]
fn validate_rejects_idempotency_ttl_above_ceiling() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        idempotency_ttl_secs: MAX_IDEMPOTENCY_TTL_SECS + 1,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "idempotency_ttl_secs exceeding MAX_IDEMPOTENCY_TTL_SECS must be rejected"
    );
}

#[test]
fn validate_accepts_default_config_idempotency_ttl_ceiling() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.idempotency_ttl_secs <= MAX_IDEMPOTENCY_TTL_SECS,
        "sanity: the shipped default_idempotency_ttl_secs must not itself exceed the ceiling"
    );
    assert!(cfg.validate().is_ok());
}

// ── previous_signing_public_keys (signing_key_seed rotation, thread #35) ───
//
// The finalize/report-part callback verifier's own dedupe-against-the-
// current-key step lives in `FileService::with_previous_signing_public_keys`
// (the current key isn't known here -- it's only derived from
// `signing_key_seed` in `gear.rs`); `validate()` only checks that each entry
// is well-formed.

#[test]
fn default_previous_signing_public_keys_is_empty() {
    assert!(
        FileStorageConfig::default()
            .previous_signing_public_keys
            .is_empty()
    );
}

#[test]
fn validate_accepts_empty_previous_signing_public_keys() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        previous_signing_public_keys: Vec::new(),
        ..FileStorageConfig::default()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn validate_accepts_well_formed_previous_signing_public_keys() {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let key = crate::infra::signed_url::Issuer::generate(60)
        .expect("issuer")
        .public_key();
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        previous_signing_public_keys: vec![URL_SAFE_NO_PAD.encode(key)],
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "a validly-formed (base64url, 32-byte) previous key must be accepted"
    );
}

#[test]
fn validate_rejects_non_base64_previous_signing_public_key() {
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        previous_signing_public_keys: vec!["not-valid-base64!!!".to_owned()],
        ..FileStorageConfig::default()
    };
    assert!(
        cfg.validate().is_err(),
        "a non-base64url entry must fail gear init, not surface lazily at the first callback"
    );
}

#[test]
fn validate_rejects_wrong_length_previous_signing_public_key() {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    // 8 bytes -- well short of the 32 an Ed25519 public key requires.
    let cfg = FileStorageConfig {
        require_signing_key_seed: false,
        previous_signing_public_keys: vec![URL_SAFE_NO_PAD.encode([1, 2, 3, 4, 5, 6, 7, 8])],
        ..FileStorageConfig::default()
    };
    let err = cfg
        .validate()
        .expect_err("a wrong-length previous key must fail gear init");
    assert!(
        err.to_string().contains("length"),
        "error should name the length mismatch: {err}"
    );
}
