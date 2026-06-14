//! Unit tests for the SDK error vocabulary:
//!
//! Per-arm verification of the `From<UsageCollectorPluginError> for
//! UsageCollectorError` dispatch-boundary translation, plus a
//! table-driven exhaustiveness fence that ensures no plugin variant is
//! silently missed when a new one lands.

use std::collections::HashSet;

use uuid::Uuid;

use super::{UsageCollectorError, UsageCollectorPluginError};
use crate::models::UsageTypeGtsId;

const SAMPLE_USAGE_TYPE_ID: &str =
    "gts.cf.core.uc.usage_record.v1~cf.mini_chat._.tokens_consumed.v1";

fn sample_id() -> UsageTypeGtsId {
    UsageTypeGtsId::new(SAMPLE_USAGE_TYPE_ID).expect("valid usage_record-derived id")
}

fn sample_uuid() -> Uuid {
    Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("sample uuid")
}

// ---------------------------------------------------------------------------
// Plugin → SDK dispatch-boundary mapping (per arm)
// ---------------------------------------------------------------------------

#[test]
fn plugin_transient_lifts_to_service_unavailable_preserving_detail() {
    // The plugin SPI's retryable channel: `Transient(detail)` lifts to the
    // retryable `ServiceUnavailable { detail, retry_after_seconds: None }`
    // envelope so callers observe it via `is_retryable()`.
    let plugin = UsageCollectorPluginError::transient("downstream connection reset");
    let lifted: UsageCollectorError = plugin.into();
    match &lifted {
        UsageCollectorError::ServiceUnavailable {
            detail,
            retry_after_seconds,
        } => {
            assert_eq!(detail, "downstream connection reset");
            assert_eq!(*retry_after_seconds, None);
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
    assert!(
        lifted.is_retryable(),
        "Transient must classify as retryable"
    );
}

#[test]
fn plugin_internal_lifts_to_sdk_internal_preserving_detail() {
    // Plugin-side `Internal` is the non-retryable catch-all and lifts 1:1
    // to the unclassified `UsageCollectorError::Internal` envelope
    // (HTTP 500). Retryable conditions go through `Transient` instead.
    let plugin = UsageCollectorPluginError::internal("invariant violation: x");
    let lifted: UsageCollectorError = plugin.into();
    match &lifted {
        UsageCollectorError::Internal(detail) => {
            assert_eq!(detail, "invariant violation: x");
        }
        other => panic!("expected Internal, got {other:?}"),
    }
    assert!(!lifted.is_retryable());
}

#[test]
fn plugin_usage_type_not_found_lifts_preserving_gts_id() {
    let gts_id = sample_id();
    let plugin = UsageCollectorPluginError::UsageTypeNotFound {
        gts_id: gts_id.clone(),
    };
    let lifted: UsageCollectorError = plugin.into();
    match &lifted {
        UsageCollectorError::UsageTypeNotFound { gts_id: id } => assert_eq!(id, &gts_id),
        other => panic!("expected UsageTypeNotFound, got {other:?}"),
    }
}

#[test]
fn plugin_usage_type_already_exists_lifts_preserving_gts_id() {
    let gts_id = sample_id();
    let plugin = UsageCollectorPluginError::UsageTypeAlreadyExists {
        gts_id: gts_id.clone(),
    };
    let lifted: UsageCollectorError = plugin.into();
    match &lifted {
        UsageCollectorError::UsageTypeAlreadyExists { gts_id: id } => assert_eq!(id, &gts_id),
        other => panic!("expected UsageTypeAlreadyExists, got {other:?}"),
    }
}

#[test]
fn plugin_usage_type_referenced_lifts_preserving_sample_ref_count() {
    let gts_id = sample_id();
    let plugin = UsageCollectorPluginError::UsageTypeReferenced {
        gts_id: gts_id.clone(),
        sample_ref_count: 7,
    };
    let lifted: UsageCollectorError = plugin.into();
    match &lifted {
        UsageCollectorError::UsageTypeReferenced {
            gts_id: id,
            sample_ref_count,
        } => {
            assert_eq!(id, &gts_id);
            assert_eq!(*sample_ref_count, 7);
        }
        other => panic!("expected UsageTypeReferenced, got {other:?}"),
    }
}

#[test]
fn plugin_idempotency_conflict_lifts_preserving_key_and_existing_uuid() {
    let existing_uuid = sample_uuid();
    let plugin = UsageCollectorPluginError::IdempotencyConflict {
        idempotency_key: "k-1".to_owned(),
        existing_uuid,
    };
    let lifted: UsageCollectorError = plugin.into();
    match &lifted {
        UsageCollectorError::IdempotencyConflict {
            idempotency_key,
            existing_uuid: bound,
        } => {
            assert_eq!(idempotency_key, "k-1");
            assert_eq!(*bound, existing_uuid);
        }
        other => panic!("expected IdempotencyConflict, got {other:?}"),
    }
}

#[test]
fn plugin_usage_record_not_found_lifts_preserving_id() {
    let id = sample_uuid();
    let plugin = UsageCollectorPluginError::UsageRecordNotFound { id };
    let lifted: UsageCollectorError = plugin.into();
    match &lifted {
        UsageCollectorError::UsageRecordNotFound { id: target } => assert_eq!(*target, id),
        other => panic!("expected UsageRecordNotFound, got {other:?}"),
    }
}

#[test]
fn plugin_usage_record_already_inactive_lifts_to_sdk_already_inactive() {
    let id = sample_uuid();
    let plugin = UsageCollectorPluginError::UsageRecordAlreadyInactive { id };
    let lifted: UsageCollectorError = plugin.into();
    match &lifted {
        UsageCollectorError::AlreadyInactive { id: target } => assert_eq!(*target, id),
        other => panic!("expected AlreadyInactive, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Plugin → SDK dispatch coverage fence
// ---------------------------------------------------------------------------

// Hand-authored table mapping every [`UsageCollectorPluginError`] variant to
// the SDK envelope it MUST lift to. Counterpart to the per-arm tests above:
// the per-arm tests pin field-level fidelity; this table-driven fence
// guarantees no plugin variant is silently missed when a new one lands.
fn plugin_lift_dispatch_cases() -> Vec<(UsageCollectorPluginError, UsageCollectorError)> {
    use UsageCollectorError as E;
    use UsageCollectorPluginError as P;

    let g = sample_id();
    let u = sample_uuid();

    vec![
        (
            P::Transient("downstream timeout".into()),
            E::ServiceUnavailable {
                detail: "downstream timeout".into(),
                retry_after_seconds: None,
            },
        ),
        (P::Internal("x".into()), E::Internal("x".into())),
        (
            P::UsageTypeNotFound { gts_id: g.clone() },
            E::UsageTypeNotFound { gts_id: g.clone() },
        ),
        (
            P::UsageTypeAlreadyExists { gts_id: g.clone() },
            E::UsageTypeAlreadyExists { gts_id: g.clone() },
        ),
        (
            P::UsageTypeReferenced {
                gts_id: g.clone(),
                sample_ref_count: 1,
            },
            E::UsageTypeReferenced {
                gts_id: g,
                sample_ref_count: 1,
            },
        ),
        (
            P::IdempotencyConflict {
                idempotency_key: "k".into(),
                existing_uuid: u,
            },
            E::IdempotencyConflict {
                idempotency_key: "k".into(),
                existing_uuid: u,
            },
        ),
        (
            P::UsageRecordNotFound { id: u },
            E::UsageRecordNotFound { id: u },
        ),
        (
            P::UsageRecordAlreadyInactive { id: u },
            E::AlreadyInactive { id: u },
        ),
    ]
}

#[test]
fn plugin_lift_dispatches_every_plugin_variant() {
    use UsageCollectorPluginError as P;

    // Compile-time exhaustiveness fence: adding a new variant to
    // UsageCollectorPluginError forces a new arm in this match and signals
    // the developer to also add a row to `plugin_lift_dispatch_cases`.
    const _EXHAUSTIVENESS_FENCE: fn(&UsageCollectorPluginError) = |err| match err {
        P::Transient(_)
        | P::Internal(_)
        | P::UsageTypeNotFound { .. }
        | P::UsageTypeAlreadyExists { .. }
        | P::UsageTypeReferenced { .. }
        | P::IdempotencyConflict { .. }
        | P::UsageRecordNotFound { .. }
        | P::UsageRecordAlreadyInactive { .. } => (),
    };

    let cases = plugin_lift_dispatch_cases();

    // Runtime fence: each plugin variant appears at most once in `cases`.
    let mut seen: HashSet<std::mem::Discriminant<P>> = HashSet::new();
    for (err, _) in &cases {
        assert!(
            seen.insert(std::mem::discriminant(err)),
            "duplicate variant in plugin-lift dispatch cases for {err:?}"
        );
    }

    // Dispatch assertion: every plugin variant lifts to the expected SDK
    // envelope. Discriminant equality is sufficient here — the per-arm
    // tests pin field-level round-trip.
    for (plugin_err, expected) in cases {
        let lifted: UsageCollectorError = plugin_err.into();
        assert_eq!(
            std::mem::discriminant(&lifted),
            std::mem::discriminant(&expected),
            "plugin lift produced unexpected SDK variant: lifted={lifted:?}, expected={expected:?}",
        );
    }
}
