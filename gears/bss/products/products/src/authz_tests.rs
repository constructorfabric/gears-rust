//! Tests for the registry's authz descriptors, label stub schemas, and the
//! [`access_scope`] PEP gate.
//!
//! The permit/deny/unavailable paths are exercised against a fake
//! `AuthZResolverApi` rather than a live resolver — the same technique the
//! sibling ledger gear's own `authz_tests.rs` uses (see
//! `gears/bss/ledger/ledger/src/authz_tests.rs`), so `access_scope`'s own
//! logic (the `EnforcerError` → `AuthzError` split, and the write-path
//! cross-tenant membership assertion) is proven without a resolver
//! deployment.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use super::{AuthzError, access_scope, actions, authz_label_type_schemas, labels, resource_types};
use crate::test_support::flat_in_enforcer;

/// RT-09: every label `mod labels` declares joins `labels::ALL`, and every resource type names one
/// of them. A census of the module's source: a label declared without joining `ALL` changes the
/// count here, which reading the definitions back could not show.
#[test]
fn every_declared_label_joins_all_and_each_resource_type_names_one() {
    let source = include_str!("authz.rs");
    let module = |head: &str| {
        let body = &source[source.find(head).unwrap()..];
        body[..body.find("\n}").unwrap()].to_owned()
    };
    let declared = module("pub mod labels {").matches("gts_id!(").count();
    let distinct: std::collections::BTreeSet<_> = labels::ALL.iter().collect();
    assert_eq!(distinct.len(), labels::ALL.len(), "ALL names a label twice");
    assert_eq!(
        declared,
        labels::ALL.len(),
        "a declared label is not in ALL"
    );
    let types = module("pub mod resource_types {");
    assert_eq!(
        types.matches("ResourceType::from_static(").count(),
        labels::ALL.len()
    );
    for rt in [
        resource_types::SKU,
        resource_types::CATEGORY,
        resource_types::APPROVAL_UNIT,
        resource_types::DERIVED_USAGE_TYPE,
    ] {
        assert!(labels::ALL.contains(&rt.name()), "{}", rt.name());
    }
}
/// Five actions on the SKU, the category and the approval unit, `reference` on the SKU, and one,
/// `author`, on the derived usage type, whose reads are `sku:read` (P-D-231, O-3).
#[test]
fn seventeen_permissions_cover_exactly_the_declared_pairs_and_inventory() {
    let all = crate::gts::permissions::all();
    assert_eq!(all.len(), 17);
    let actual = all
        .iter()
        .map(|p| (p.resource_type.as_str(), p.action.as_str()))
        .collect::<std::collections::BTreeSet<_>>();
    let mut expected = std::collections::BTreeSet::new();
    for label in [labels::SKU, labels::CATEGORY, labels::APPROVAL_UNIT] {
        for action in [
            actions::READ,
            actions::AUTHOR,
            actions::SUBMIT,
            actions::APPROVE,
            actions::SETTINGS,
        ] {
            expected.insert((label, action));
        }
    }
    expected.insert((labels::SKU, actions::REFERENCE));
    expected.insert((labels::DERIVED_USAGE_TYPE, actions::AUTHOR));
    assert_eq!(actual, expected);
    let prefix = gts_id!("cf.toolkit.authz.permission.v1~");
    let inventory = toolkit_gts::inventory::iter::<toolkit_gts::InventoryInstance>
        .into_iter()
        .filter(|e| {
            e.instance_id
                .starts_with(&format!("{prefix}cf.bss.products."))
        })
        .collect::<Vec<_>>();
    assert_eq!(inventory.len(), 17);
    for p in all {
        let id = p.id.to_string();
        let entry = inventory.iter().find(|e| e.instance_id == id).unwrap();
        assert_eq!(entry.type_id, prefix);
        assert_eq!((entry.payload_fn)(), serde_json::to_value(p).unwrap());
    }
}
#[test]
fn action_names_are_pairwise_distinct() {
    assert_eq!(
        actions::ALL,
        &[
            "read",
            "author",
            "submit",
            "approve",
            "settings",
            "reference"
        ]
    );
    let distinct = actions::ALL
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(distinct.len(), 6);
}
/// Stronger than a suffix match: every authz label must parse as a
/// structurally valid GTS id AND be a concrete TYPE id (type ids end `~`).
#[test]
fn labels_are_concrete_gts_types() {
    for label in labels::ALL {
        assert!(
            ::gts::GtsId::try_new(label).is_ok(),
            "label {label} is not a structurally valid GTS id"
        );
        assert!(
            label.ends_with('~'),
            "label {label} must be a concrete type id"
        );
    }
}

/// One stub schema per label, each addressed at the label's own `$id` and
/// shaped as a bare JSON-Schema object — the shape the platform RBAC
/// role-definition validator resolves a `target_type` against.
#[test]
fn authz_label_type_schemas_covers_every_label_exactly_once() {
    let schemas = authz_label_type_schemas();
    assert_eq!(schemas.len(), labels::ALL.len());

    let ids: std::collections::BTreeSet<String> = schemas
        .iter()
        .map(|schema| {
            schema["$id"]
                .as_str()
                .expect("each stub schema carries a $id")
                .to_owned()
        })
        .collect();
    let expected: std::collections::BTreeSet<String> = labels::ALL
        .iter()
        .map(|label| format!("gts://{label}"))
        .collect();
    assert_eq!(ids, expected);

    for schema in &schemas {
        assert_eq!(schema["type"], "object");
    }
}

fn ctx_for(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .subject_type(gts_id!("cf.core.security.subject_user.v1~"))
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("authed SecurityContext must build")
}

/// A write gate (a target `owner_tenant_id`; constraints are always required)
/// must DENY when the target tenant is outside the PDP's compiled scope, and
/// ALLOW when it is inside. This pins the cross-tenant-write hole: the
/// degraded flat-`In` decision does not re-validate `owner_tenant_id` at the
/// PDP, so the gate itself must assert target membership.
#[tokio::test]
async fn write_gate_denies_target_outside_authorized_scope() {
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let enforcer = flat_in_enforcer(tenant_a); // authorized for tenant_a only
    let ctx = ctx_for(tenant_a);

    // Cross-tenant write: target B is outside the authorized In([A]) -> Denied.
    let denied = access_scope(
        &enforcer,
        &ctx,
        &resource_types::SKU,
        actions::AUTHOR,
        Some(tenant_b),
    )
    .await;
    assert!(
        matches!(denied, Err(AuthzError::Denied(_))),
        "writing into tenant B with scope In([A]) must be denied, got {denied:?}"
    );

    // In-scope write: target A is inside the authorized scope -> allowed, and
    // the returned scope carries the In([A]) filter for SQL-level binding.
    let allowed = access_scope(
        &enforcer,
        &ctx,
        &resource_types::SKU,
        actions::AUTHOR,
        Some(tenant_a),
    )
    .await
    .expect("writing into own tenant A must be allowed");
    assert!(
        allowed.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_a),
        "the granted scope must carry the tenant-A filter"
    );
}

/// PDP fake that always fails to evaluate (models an unreachable PDP).
struct FailingResolver;

#[async_trait]
impl AuthZResolverApi for FailingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err(CanonicalError::internal("pdp unreachable".to_owned()).create())
    }
}

/// PDP fake that explicitly denies (`decision = false`).
struct DenyingResolver;

#[async_trait]
impl AuthZResolverApi for DenyingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: vec![],
                deny_reason: None,
            },
        })
    }
}

/// An unreachable PDP must fail closed as `Unavailable` (→ 503), NOT `Denied`
/// (→ 403): the two carry different operator semantics and retry behaviour.
#[tokio::test]
async fn pdp_evaluation_failure_maps_to_unavailable() {
    let enforcer = PolicyEnforcer::new(Arc::new(FailingResolver));
    let ctx = ctx_for(Uuid::now_v7());
    let res = access_scope(&enforcer, &ctx, &resource_types::SKU, actions::READ, None).await;
    assert!(
        matches!(res, Err(AuthzError::Unavailable(_))),
        "an unreachable PDP must fail closed as Unavailable, got {res:?}"
    );
}

/// An explicit PDP deny maps to `Denied` (→ 403).
#[tokio::test]
async fn pdp_decision_false_maps_to_denied() {
    let enforcer = PolicyEnforcer::new(Arc::new(DenyingResolver));
    let ctx = ctx_for(Uuid::now_v7());
    let res = access_scope(&enforcer, &ctx, &resource_types::SKU, actions::READ, None).await;
    assert!(
        matches!(res, Err(AuthzError::Denied(_))),
        "an explicit PDP deny must map to Denied, got {res:?}"
    );
}

/// A read (`owner_tenant_id = None`) skips the write-membership assertion and
/// returns the PDP's compiled `In([tenant])` scope verbatim for SQL binding.
#[tokio::test]
async fn read_path_returns_pdp_scope_without_membership_check() {
    let tenant = Uuid::now_v7();
    let enforcer = flat_in_enforcer(tenant);
    let ctx = ctx_for(tenant);
    let scope = access_scope(&enforcer, &ctx, &resource_types::SKU, actions::READ, None)
        .await
        .expect("read must be allowed");
    assert!(
        scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant),
        "the read scope must carry the tenant filter"
    );
}

/// PDP fake that allows under a constraint on a property this gear does not support, so the
/// constraints do not compile.
struct UncompilableResolver;

#[async_trait]
impl AuthZResolverApi for UncompilableResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        "a_property_the_gear_never_named",
                        vec![Uuid::now_v7()],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// RS-08: constraints that do not compile are a denial whose reason is a fixed token; the
/// compiler's diagnostic, which names the PDP's properties, stays in the log.
#[tokio::test]
async fn uncompilable_constraints_deny_with_a_fixed_reason() {
    let enforcer = PolicyEnforcer::new(Arc::new(UncompilableResolver));
    let ctx = ctx_for(Uuid::now_v7());
    let res = access_scope(&enforcer, &ctx, &resource_types::SKU, actions::READ, None).await;
    match res {
        Err(AuthzError::Denied(reason)) => {
            assert_eq!(reason, super::CONSTRAINT_COMPILATION_FAILED);
        }
        other => panic!("uncompilable constraints must deny, got {other:?}"),
    }
}
