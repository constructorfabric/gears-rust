//! Unit tests for [`crate::domain::bootstrap::config::BootstrapConfig`].

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use std::time::Duration;
use toolkit_gts::gts_id;

use super::*;

#[test]
fn deserialize_empty_table_yields_defaults() {
    let cfg: BootstrapConfig = serde_json::from_str("{}").expect("ok");
    assert_eq!(cfg.idp_wait_timeout, Duration::from_mins(5));
    assert_eq!(cfg.idp_retry_backoff_initial, Duration::from_secs(2));
    assert_eq!(cfg.idp_retry_backoff_max, Duration::from_secs(30));
    assert!(!cfg.strict);
}

#[test]
fn deserialize_overrides() {
    let cfg: BootstrapConfig =
        serde_json::from_str(r#"{"root_id":"00000000-0000-0000-0000-0000000000aa","strict":true}"#)
            .expect("ok");
    assert!(cfg.strict);
    assert_eq!(cfg.root_id, Uuid::from_u128(0xAA));
}

#[test]
fn validate_default_rejects_nil_identifiers() {
    // An empty TOML table deserialises to `Default::default()`,
    // which carries `Uuid::nil()` for `root_id` and an empty
    // `root_tenant_type`. The validator MUST reject this so
    // `strict = true` deployments cannot insert a nil-id root
    // and break idempotency on the next restart.
    let cfg = BootstrapConfig::default();
    let err = cfg.validate().expect_err("nil ids must reject");
    assert!(err.contains("root_id"), "got: {err}");
    assert!(err.contains("root_tenant_type"), "got: {err}");
}

#[test]
fn validate_accepts_fully_specified_config() {
    let cfg = BootstrapConfig {
        root_id: Uuid::from_u128(0xAA),
        root_name: "platform-root".into(),
        root_tenant_type: gts::GtsTypeId::new(gts_id!(
            "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
        )),
        root_tenant_metadata: None,
        idp_wait_timeout: Duration::from_mins(5),
        idp_retry_backoff_initial: Duration::from_secs(2),
        idp_retry_backoff_max: Duration::from_secs(30),
        strict: true,
    };
    cfg.validate().expect("fully-specified config is valid");
}

#[test]
fn validate_rejects_zero_idp_wait_timeout() {
    let cfg = BootstrapConfig {
        root_id: Uuid::from_u128(0xAA),
        root_name: "platform-root".into(),
        root_tenant_type: gts::GtsTypeId::new(gts_id!(
            "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
        )),
        root_tenant_metadata: None,
        idp_wait_timeout: Duration::ZERO,
        idp_retry_backoff_initial: Duration::from_secs(2),
        idp_retry_backoff_max: Duration::from_secs(30),
        strict: true,
    };
    let err = cfg
        .validate()
        .expect_err("zero idp_wait_timeout must reject");
    assert!(err.contains("idp_wait_timeout"), "got: {err}");
    assert!(err.contains("> 0"), "got: {err}");
}

#[test]
fn validate_rejects_idp_wait_timeout_above_cap() {
    let cfg = BootstrapConfig {
        root_id: Uuid::from_u128(0xAA),
        root_name: "platform-root".into(),
        root_tenant_type: gts::GtsTypeId::new(gts_id!(
            "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
        )),
        root_tenant_metadata: None,
        // One past the documented cap; with no upper bound the
        // deadline math `Instant::now() + idp_wait_timeout` and the
        // cast `i64::try_from(secs * 2)` would both go unchecked.
        idp_wait_timeout: MAX_IDP_WAIT_TIMEOUT + Duration::from_secs(1),
        idp_retry_backoff_initial: Duration::from_secs(2),
        idp_retry_backoff_max: Duration::from_secs(30),
        strict: true,
    };
    let err = cfg
        .validate()
        .expect_err("idp_wait_timeout above cap must reject");
    assert!(err.contains("idp_wait_timeout"), "got: {err}");
    assert!(err.contains("<= 1h"), "got: {err}");
}

#[test]
fn validate_accepts_idp_wait_timeout_at_cap() {
    let cfg = BootstrapConfig {
        root_id: Uuid::from_u128(0xAA),
        root_name: "platform-root".into(),
        root_tenant_type: gts::GtsTypeId::new(gts_id!(
            "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
        )),
        root_tenant_metadata: None,
        idp_wait_timeout: MAX_IDP_WAIT_TIMEOUT,
        idp_retry_backoff_initial: Duration::from_secs(2),
        idp_retry_backoff_max: Duration::from_secs(30),
        strict: true,
    };
    cfg.validate().expect("value at cap must be accepted");
}

#[test]
fn validate_rejects_inverted_backoff_envelope() {
    let cfg = BootstrapConfig {
        root_id: Uuid::from_u128(0xAA),
        root_name: "platform-root".into(),
        root_tenant_type: gts::GtsTypeId::new(gts_id!(
            "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
        )),
        root_tenant_metadata: None,
        idp_wait_timeout: Duration::from_mins(5),
        idp_retry_backoff_initial: Duration::from_mins(1),
        idp_retry_backoff_max: Duration::from_secs(30),
        strict: true,
    };
    let err = cfg.validate().expect_err("max < initial must reject");
    assert!(err.contains("idp_retry_backoff_max"), "got: {err}");
}
