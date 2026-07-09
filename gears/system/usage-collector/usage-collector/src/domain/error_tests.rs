//! Unit tests for the host `DomainError` -> SDK error bridge.
//!
//! Coverage mirrors the canonical mapping: plugin variants lift into
//! `DomainError`, and `DomainError` projects forward onto the compacted
//! seven-category `UsageCollectorError` envelope. The reverse
//! (`UsageCollectorError -> DomainError`) bridge was removed in the
//! error-envelope compaction — the public envelope is terminal — so these
//! tests exercise the plugin->domain and domain->SDK directions only.

use usage_collector_sdk::{
    ConflictReason, USAGE_RECORD_RESOURCE, USAGE_TYPE_RESOURCE, UsageCollectorError,
    UsageCollectorPluginError, UsageTypeGtsId, ValidationReason,
};

use super::*;

const SAMPLE_USAGE_TYPE_ID: &str =
    "gts.cf.core.uc.usage_record.v1~cf.mini_chat._.tokens_consumed.v1";

fn sample_gts_id() -> UsageTypeGtsId {
    UsageTypeGtsId::new(SAMPLE_USAGE_TYPE_ID).expect("valid usage_record-derived usage-type gts_id")
}

#[test]
fn plugin_transient_maps_to_service_unavailable() {
    let domain: DomainError =
        UsageCollectorPluginError::transient("downstream connection reset").into();
    assert!(matches!(
        &domain,
        DomainError::PluginTransient { detail, retry_after_seconds: None }
            if detail == "downstream connection reset"
    ));
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::ServiceUnavailable {
            retry_after_seconds,
            detail,
        } => {
            assert_eq!(detail, "downstream connection reset");
            assert_eq!(retry_after_seconds, None);
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[test]
fn plugin_internal_maps_to_internal() {
    let domain: DomainError = UsageCollectorPluginError::internal("invariant violation").into();
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::Internal { detail } => assert_eq!(detail, "invariant violation"),
        other => panic!("expected Internal, got {other:?}"),
    }
}

#[test]
fn authorization_denied_lifts_to_permission_denied_preserving_reason() {
    let domain = DomainError::AuthorizationDenied {
        reason: Some("denied by policy".to_owned()),
    };
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::PermissionDenied { detail } => {
            assert_eq!(detail, "denied by policy");
        }
        other => panic!("expected PermissionDenied, got {other:?}"),
    }
}

#[test]
fn enforcer_denied_extracts_deny_reason_fields_into_reason_string() {
    use authz_resolver_sdk::EnforcerError;
    use authz_resolver_sdk::models::DenyReason;

    let with_details: DomainError = EnforcerError::Denied {
        deny_reason: Some(DenyReason {
            error_code: "TENANT_BARRIER".to_owned(),
            details: Some("subject home tenant != context".to_owned()),
        }),
    }
    .into();
    match with_details {
        DomainError::AuthorizationDenied { reason } => assert_eq!(
            reason.as_deref(),
            Some("TENANT_BARRIER: subject home tenant != context"),
        ),
        other => panic!("expected AuthorizationDenied, got {other:?}"),
    }

    let bare: DomainError = EnforcerError::Denied {
        deny_reason: Some(DenyReason {
            error_code: "FORBIDDEN".to_owned(),
            details: None,
        }),
    }
    .into();
    match bare {
        DomainError::AuthorizationDenied { reason } => {
            assert_eq!(reason.as_deref(), Some("FORBIDDEN"));
        }
        other => panic!("expected AuthorizationDenied, got {other:?}"),
    }

    let missing: DomainError = EnforcerError::Denied { deny_reason: None }.into();
    match missing {
        DomainError::AuthorizationDenied { reason } => assert!(reason.is_none()),
        other => panic!("expected AuthorizationDenied, got {other:?}"),
    }
}

#[test]
fn plugin_not_found_maps_to_service_unavailable() {
    let domain = DomainError::PluginNotFound {
        vendor: "acme".to_owned(),
    };
    let sdk: UsageCollectorError = domain.into();
    assert!(matches!(
        sdk,
        UsageCollectorError::ServiceUnavailable { .. }
    ));
}

#[test]
fn domain_usage_type_not_found_lifts_to_sdk_not_found() {
    let gts_id = sample_gts_id();
    let domain = DomainError::UsageTypeNotFound {
        gts_id: gts_id.clone(),
    };
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::NotFound {
            resource_type,
            name,
            ..
        } => {
            assert_eq!(resource_type, USAGE_TYPE_RESOURCE);
            assert_eq!(name, gts_id.as_ref());
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn plugin_usage_type_not_found_lifts_to_domain_variant() {
    let gts_id = sample_gts_id();
    let domain: DomainError = UsageCollectorPluginError::UsageTypeNotFound {
        gts_id: gts_id.clone(),
    }
    .into();
    assert!(matches!(
        &domain,
        DomainError::UsageTypeNotFound { gts_id: g } if g == &gts_id
    ));
}

#[test]
fn domain_usage_type_already_exists_lifts_to_sdk_already_exists() {
    let gts_id = sample_gts_id();
    let domain = DomainError::UsageTypeAlreadyExists {
        gts_id: gts_id.clone(),
    };
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::AlreadyExists {
            resource_type,
            name,
            ..
        } => {
            assert_eq!(resource_type, USAGE_TYPE_RESOURCE);
            assert_eq!(name, gts_id.as_ref());
        }
        other => panic!("expected AlreadyExists, got {other:?}"),
    }
}

#[test]
fn plugin_usage_type_already_exists_lifts_to_domain_variant() {
    let gts_id = sample_gts_id();
    let domain: DomainError = UsageCollectorPluginError::UsageTypeAlreadyExists {
        gts_id: gts_id.clone(),
    }
    .into();
    assert!(matches!(
        &domain,
        DomainError::UsageTypeAlreadyExists { gts_id: g } if g == &gts_id
    ));
}

#[test]
fn plugin_usage_type_referenced_lifts_to_sdk_conflict() {
    let gts_id = sample_gts_id();
    let domain: DomainError = UsageCollectorPluginError::UsageTypeReferenced {
        gts_id: gts_id.clone(),
        sample_ref_count: 42,
    }
    .into();
    assert!(matches!(
        &domain,
        DomainError::UsageTypeReferenced { gts_id: g, sample_ref_count: 42 } if g == &gts_id
    ));
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::Conflict {
            resource_type,
            name,
            reason,
            detail,
        } => {
            assert_eq!(resource_type, USAGE_TYPE_RESOURCE);
            assert_eq!(name, gts_id.as_ref());
            assert_eq!(reason, ConflictReason::UsageTypeReferenced);
            assert!(detail.contains("referenced by 42 samples"));
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn domain_unknown_metadata_key_lifts_to_invalid_argument() {
    let gts_id = sample_gts_id();
    let key = "unexpected_field".to_owned();
    let domain = DomainError::UnknownMetadataKey {
        gts_id: gts_id.clone(),
        key: key.clone(),
    };
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::InvalidArgument {
            resource_type,
            resource_name,
            reason,
            detail,
            ..
        } => {
            assert_eq!(resource_type, USAGE_TYPE_RESOURCE);
            assert_eq!(resource_name.as_deref(), Some(gts_id.as_ref()));
            assert_eq!(reason, ValidationReason::UnknownMetadataKey);
            assert!(detail.contains(&key));
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[test]
fn idempotency_conflict_lifts_to_conflict_keyed_by_existing_uuid() {
    let existing_uuid = uuid::Uuid::from_u128(0x1234_5678);
    let key = "idem-1".to_owned();
    let domain: DomainError = UsageCollectorPluginError::IdempotencyConflict {
        idempotency_key: key.clone(),
        existing_uuid,
    }
    .into();
    assert!(matches!(
        &domain,
        DomainError::IdempotencyConflict { idempotency_key: ik, existing_uuid: u }
            if ik == &key && *u == existing_uuid
    ));
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::Conflict {
            resource_type,
            name,
            reason,
            ..
        } => {
            assert_eq!(resource_type, USAGE_RECORD_RESOURCE);
            assert_eq!(name, existing_uuid.to_string());
            assert_eq!(reason, ConflictReason::IdempotencyConflict);
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

// ── event-deactivation feature: deactivate-record error variants ────

#[test]
fn plugin_usage_record_not_found_lifts_to_sdk_not_found() {
    let id = uuid::Uuid::from_u128(0xDEAD_BEEF);
    let domain: DomainError = UsageCollectorPluginError::UsageRecordNotFound { id }.into();
    assert!(matches!(domain, DomainError::UsageRecordNotFound { id: d } if d == id));
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::NotFound {
            resource_type,
            name,
            ..
        } => {
            assert_eq!(resource_type, USAGE_RECORD_RESOURCE);
            assert_eq!(name, id.to_string());
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn plugin_usage_record_already_inactive_lifts_to_sdk_conflict() {
    let id = uuid::Uuid::from_u128(0xCAFE_BABE);
    let domain: DomainError = UsageCollectorPluginError::UsageRecordAlreadyInactive { id }.into();
    assert!(matches!(domain, DomainError::UsageRecordAlreadyInactive { id: d } if d == id));
    let sdk: UsageCollectorError = domain.into();
    match sdk {
        UsageCollectorError::Conflict {
            resource_type,
            name,
            reason,
            ..
        } => {
            assert_eq!(resource_type, USAGE_RECORD_RESOURCE);
            assert_eq!(name, id.to_string());
            assert_eq!(reason, ConflictReason::AlreadyInactive);
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn sdk_already_inactive_is_not_retryable() {
    let err = UsageCollectorError::already_inactive(uuid::Uuid::nil());
    assert!(!err.is_retryable(), "AlreadyInactive is not retryable");
}
