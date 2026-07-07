//! Unit tests for the pure parts of the cross-tenant elevation gateway: the
//! degraded-fallback `subtree_read_scope`, the `TargetScope` value, and the
//! shared `resolve_action_scope` elevation contract (branch order). The
//! transaction-bound branches (`resolve_read_scope`) need a database and are
//! exercised by `tests/postgres_cross_tenant.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use toolkit_db::secure::AccessScope;
use toolkit_security::pep_properties;
use uuid::Uuid;

use super::{TargetScope, resolve_action_scope, subtree_read_scope};
use crate::domain::error::DomainError;

#[test]
fn subtree_read_scope_returns_home_tenant_only() {
    let home = Uuid::now_v7();
    let other = Uuid::now_v7();
    let scope = subtree_read_scope(home);
    // The degraded fallback authorizes the home tenant and nothing else.
    assert!(
        scope.contains_uuid(pep_properties::OWNER_TENANT_ID, home),
        "subtree fallback must authorize the home tenant"
    );
    assert!(
        !scope.contains_uuid(pep_properties::OWNER_TENANT_ID, other),
        "subtree fallback must NOT authorize any other tenant (own-tenant-only)"
    );
}

#[test]
fn target_scope_carries_tenant_id() {
    let t = Uuid::now_v7();
    let target = TargetScope { tenant_id: t };
    assert_eq!(target.tenant_id, t);
}

#[test]
fn resolve_action_scope_routine_returns_home_scope() {
    let home = Uuid::now_v7();
    let home_scope = AccessScope::for_tenant(home);
    // No target → routine; no reason required.
    let (t, _) = resolve_action_scope(home, &home_scope, None, true, None).unwrap();
    assert_eq!(t, home);
    // Target == home → routine.
    let (t2, _) = resolve_action_scope(
        home,
        &home_scope,
        Some(TargetScope { tenant_id: home }),
        true,
        None,
    )
    .unwrap();
    assert_eq!(t2, home);
}

#[test]
fn resolve_action_scope_checks_role_before_reason() {
    let home = Uuid::now_v7();
    let target = Uuid::now_v7();
    let home_scope = AccessScope::for_tenant(home);
    // Unauthorized AND no reason → the role denial wins (role checked first, §5),
    // so an unauthorized caller never learns whether a reason would have sufficed.
    let err = resolve_action_scope(
        home,
        &home_scope,
        Some(TargetScope { tenant_id: target }),
        false,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, DomainError::CrossTenantAccessDenied(_)));
}

#[test]
fn resolve_action_scope_requires_reason_when_authorized() {
    let home = Uuid::now_v7();
    let target = Uuid::now_v7();
    let home_scope = AccessScope::for_tenant(home);
    for reason in [None, Some(""), Some("   ")] {
        let err = resolve_action_scope(
            home,
            &home_scope,
            Some(TargetScope { tenant_id: target }),
            true,
            reason,
        )
        .unwrap_err();
        assert!(
            matches!(err, DomainError::MissingInvestigationReason(_)),
            "empty/blank reason must be rejected: {reason:?}"
        );
    }
}

#[test]
fn resolve_action_scope_elevates_to_target() {
    let home = Uuid::now_v7();
    let target = Uuid::now_v7();
    let home_scope = AccessScope::for_tenant(home);
    let (t, scope) = resolve_action_scope(
        home,
        &home_scope,
        Some(TargetScope { tenant_id: target }),
        true,
        Some("dispute #4821"),
    )
    .unwrap();
    assert_eq!(t, target);
    assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, target));
    assert!(!scope.contains_uuid(pep_properties::OWNER_TENANT_ID, home));
}
