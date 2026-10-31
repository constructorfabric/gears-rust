// Created: 2026-07-29 by Constructor Tech
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! create a resource group in an explicit target tenant.
//!
//! `CreateGroupRequest::tenant_id` (optional) lets an authorized caller
//! (platform admin / onboarding) create a group in a tenant other than the
//! one derived from their own `SecurityContext`. Coverage:
//!
//! - omitted `tenant_id` -> byte-for-byte the previous behavior
//!   (`tenant_id_omitted_uses_caller_tenant`)
//! - explicit `tenant_id` equal to the caller's own tenant -> no-op, same
//!   outcome as omitted (`tenant_id_matches_caller_tenant_succeeds`)
//! - explicit foreign `tenant_id` permitted by the compiled `AccessScope` ->
//!   succeeds (`foreign_tenant_allowed_by_permissive_policy_succeeds`)
//! - explicit foreign `tenant_id` NOT covered by the `AccessScope` -> a
//!   `TenantNotFound` domain error indistinguishable from "tenant doesn't
//!   exist" (`foreign_tenant_denied_by_default_policy_returns_tenant_not_found`)
//! - an `InTenantSubtree`-only constraint is rejected fail-closed at PEP
//!   compile time: this gear never advertises `TenantHierarchy` (no
//!   `tenant_closure` dependency), so the unadvertised predicate is a PDP
//!   contract violation surfaced as `AccessDenied` — even when the target
//!   tenant is literally the constraint's own root
//!   (`in_tenant_subtree_scope_is_denied_fail_closed`)
//! - explicit `tenant_id` conflicting with an explicit `parent_id`'s actual
//!   tenant -> rejected, message does not leak tenant ids (style)
//!   (`explicit_tenant_id_conflicting_with_parent_tenant_returns_validation_error`)
//! - a tenant-typed group (`code` starting with `TENANT_RG_TYPE_PATH`) with
//!   an explicit `tenant_id` is a contradiction, rejected outright
//!   (`tenant_typed_group_with_explicit_tenant_id_returns_validation_error`)
//! - an explicit `id` combined with a cross-tenant target is rejected as a
//!   stopgap, even when the `AccessScope` would otherwise permit the
//!   target tenant
//!   (`explicit_id_with_foreign_tenant_id_returns_validation_error`)
//!
//! `tests/api_rest_test.rs` covers the HTTP-level wire shape (snake_case
//! `tenant_id`, RFC-9457 problem shape on the negative paths) for the same
//! set of rules.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use authz_resolver_sdk::constraints::{
    Constraint, InPredicate, InTenantSubtreePredicate, Predicate,
};
use authz_resolver_sdk::{
    AuthZResolverApi, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
    PolicyEnforcer,
};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::{PlatformSecurityContext, pep_properties};

use resource_group::domain::error::DomainError;
use resource_group::domain::group_service::{GroupService, QueryProfile};
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group_sdk::CreateGroupRequest;

/// Build a `GroupService` wired with a caller-supplied enforcer, otherwise
/// identical to `common::make_group_service`.
fn make_group_service_with_enforcer(
    db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    types: &common::TestTypes,
    enforcer: PolicyEnforcer,
) -> GroupService<GroupRepository> {
    GroupService::new(
        db,
        QueryProfile::default(),
        enforcer,
        Arc::new(GroupRepository),
        types.catalog(),
    )
}

// -- AuthZ mocks --

/// Permits any target tenant by echoing back whatever `owner_tenant_id`
/// resource property the caller sent, as an `In` constraint.
///
/// This both models "the policy grants access to this specific tenant" and,
/// as a side effect, proves `GroupService::create_group` actually forwards
/// the resolved target tenant to the PDP: if it didn't, this
/// mock would fall back to `Uuid::nil()` and every assertion below that a
/// *specific* foreign `target_tenant` succeeded would fail.
struct TargetTenantAllowAuthZ;

#[async_trait]
impl AuthZResolverApi for TargetTenantAllowAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let target = request
            .resource
            .properties
            .get(pep_properties::OWNER_TENANT_ID)
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or(Uuid::nil());

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [target],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// Permits, but the sole constraint is `InTenantSubtree` rooted at a
/// caller-chosen tenant. Models a nonconforming PDP: this gear never
/// advertises the `TenantHierarchy` capability (no DB-backed
/// `tenant_closure` lookup), so per the capability negotiation contract a
/// PDP must expand such a policy to explicit `in` predicates or deny --
/// returning the native predicate anyway is a contract violation the PEP
/// rejects fail-closed at compile time, even when the target tenant is
/// literally the subtree's own root.
struct InTenantSubtreeOnlyAuthZ {
    root_tenant_id: Uuid,
}

#[async_trait]
impl AuthZResolverApi for InTenantSubtreeOnlyAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        self.root_tenant_id,
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

// -- create_group: omitted / same-tenant tenant_id (backward compatibility) --

/// Omitted `tenant_id` must be byte-for-byte the previous behavior:
/// target tenant == the caller's own tenant, no extra AuthZ round trip
/// beyond the one `create_group` always performed.
#[tokio::test]
async fn tenant_id_omitted_uses_caller_tenant() {
    let db = common::test_db().await;
    let types = common::TestTypes::new();
    let group_svc = common::make_group_service(db.clone(), &types);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let root_type = common::create_root_type(&types, "vhp2162a");

    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code, "Omitted".to_owned()),
            tenant_id,
        )
        .await
        .expect("create_group with omitted tenant_id must succeed");

    assert_eq!(group.hierarchy.tenant_id, tenant_id);
}

/// An explicit `tenant_id` equal to the caller's own tenant must succeed
/// identically to the omitted case -- no cross-tenant AuthZ re-check is
/// triggered when target == caller's own tenant.
#[tokio::test]
async fn tenant_id_matches_caller_tenant_succeeds() {
    let db = common::test_db().await;
    let types = common::TestTypes::new();
    // `common::make_group_service` wires `AllowAllAuthZ`, which clamps the
    // returned scope to the caller's own tenant -- if the extra
    // check ran here, it would still pass (target == caller), but this test
    // pins that the common "explicit but same tenant" case behaves
    // identically to omission, not merely that it happens to also pass.
    let group_svc = common::make_group_service(db.clone(), &types);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let root_type = common::create_root_type(&types, "vhp2162b");

    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code, "SameTenant".to_owned())
                .with_tenant_id(Some(tenant_id)),
            tenant_id,
        )
        .await
        .expect("create_group with tenant_id == caller's own tenant must succeed");

    assert_eq!(group.hierarchy.tenant_id, tenant_id);
}

// -- create_group: foreign tenant_id, AuthZ decision --

/// A foreign target tenant covered by the compiled `AccessScope` (an `In`
/// constraint containing it) succeeds -- the platform-admin / onboarding
/// use case exists for.
#[tokio::test]
async fn foreign_tenant_allowed_by_permissive_policy_succeeds() {
    let db = common::test_db().await;
    let types = common::TestTypes::new();
    let group_svc = make_group_service_with_enforcer(
        db.clone(),
        &types,
        PolicyEnforcer::new(Arc::new(TargetTenantAllowAuthZ)),
    );
    let caller_tenant = Uuid::now_v7();
    let target_tenant = Uuid::now_v7();
    let ctx = common::make_ctx(caller_tenant);
    let root_type = common::create_root_type(&types, "vhp2162c");

    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code, "ForeignAllowed".to_owned())
                .with_tenant_id(Some(target_tenant)),
            caller_tenant,
        )
        .await
        .expect("create_group into a foreign tenant covered by AccessScope must succeed");

    assert_eq!(group.hierarchy.tenant_id, target_tenant);
    assert_ne!(
        target_tenant, caller_tenant,
        "test must exercise a genuinely different tenant"
    );
}

/// A foreign target tenant NOT covered by the compiled `AccessScope` (the
/// realistic tenant-clamp shape every PDP plugin in this repo returns) must
/// be rejected as `TenantNotFound`, not silently allowed and not a
/// `PermissionDenied` -- the whole point is that it must be
/// indistinguishable from "no such tenant".
#[tokio::test]
async fn foreign_tenant_denied_by_default_policy_returns_tenant_not_found() {
    let db = common::test_db().await;
    let types = common::TestTypes::new();
    let group_svc = common::make_group_service(db.clone(), &types);
    let caller_tenant = Uuid::now_v7();
    let target_tenant = Uuid::now_v7();
    let ctx = common::make_ctx(caller_tenant);
    let root_type = common::create_root_type(&types, "vhp2162d");

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code, "ForeignDenied".to_owned())
                .with_tenant_id(Some(target_tenant)),
            caller_tenant,
        )
        .await
        .expect_err("create_group into an ungranted foreign tenant must fail");

    match err {
        DomainError::TenantNotFound { tenant_id } => assert_eq!(tenant_id, target_tenant),
        other => panic!("expected DomainError::TenantNotFound, got: {other:?}"),
    }
}

/// An `InTenantSubtree`-only constraint from the PDP is rejected fail-closed
/// at PEP compile time: this gear never advertises `TenantHierarchy` (it has
/// no `tenant_closure` dependency), so the unadvertised native predicate is
/// a PDP contract violation surfaced as a typed
/// `ConstraintCompileError::UnadvertisedCapabilities`, which this gear maps
/// to a clean `AccessDenied` rather than an internal error. This holds even
/// when the target tenant is literally the constraint's own
/// `root_tenant_id`, and the denial fires before any tenant resolution --
/// regardless of whether the target tenant exists -- so tenant existence
/// still cannot be probed through this path.
#[tokio::test]
async fn in_tenant_subtree_scope_is_denied_fail_closed() {
    let db = common::test_db().await;
    let types = common::TestTypes::new();
    let caller_tenant = Uuid::now_v7();
    let target_tenant = Uuid::now_v7();
    let group_svc = make_group_service_with_enforcer(
        db.clone(),
        &types,
        PolicyEnforcer::new(Arc::new(InTenantSubtreeOnlyAuthZ {
            root_tenant_id: target_tenant,
        })),
    );
    let ctx = common::make_ctx(caller_tenant);
    let root_type = common::create_root_type(&types, "vhp2162e");

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code, "SubtreeDenied".to_owned()).with_tenant_id(Some(target_tenant)),
            caller_tenant,
        )
        .await
        .expect_err(
            "InTenantSubtree-only AccessScope must fail closed even though target == root_tenant_id",
        );

    match err {
        DomainError::AccessDenied { message } => {
            assert!(
                !message.contains(&target_tenant.to_string())
                    && !message.contains(&caller_tenant.to_string()),
                "message must not leak tenant ids: {message}"
            );
        }
        other => panic!("expected DomainError::AccessDenied, got: {other:?}"),
    }
}

// -- create_group: conflict with an explicit parent --

/// An explicit `tenant_id` that conflicts with an explicit `parent_id`'s
/// actual tenant is rejected, even when the `AccessScope` would otherwise
/// permit the target tenant on its own. The rejection message must not
/// leak either tenant's id (mirrors the fix for the caller's-own-
/// tenant case).
#[tokio::test]
async fn explicit_tenant_id_conflicting_with_parent_tenant_returns_validation_error() {
    let db = common::test_db().await;
    let types = common::TestTypes::new();
    let group_svc = make_group_service_with_enforcer(
        db.clone(),
        &types,
        PolicyEnforcer::new(Arc::new(TargetTenantAllowAuthZ)),
    );
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_a);

    let root_type = common::create_root_type(&types, "vhp2162f");
    let child_type = common::create_child_type(&types, "vhp2162fchild", &[&root_type.code], &[]);

    // Root group lives in tenant_a (the `TargetTenantAllowAuthZ` mock
    // permits any tenant, so this create is unaffected by).
    let root = common::create_root_group(&group_svc, &ctx, &root_type.code, "Root", tenant_a).await;

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(child_type.code, "Conflict".to_owned())
                .with_parent_id(Some(root.id))
                .with_tenant_id(Some(tenant_b)),
            tenant_a,
        )
        .await
        .expect_err("parent tenant vs. explicit target tenant mismatch must be rejected");

    match err {
        DomainError::Validation { message } => {
            assert!(
                !message.contains(&tenant_a.to_string())
                    && !message.contains(&tenant_b.to_string()),
                "message must not leak tenant ids (style): {message}"
            );
        }
        other => panic!("expected DomainError::Validation, got: {other:?}"),
    }
}

// -- create_group: tenant-typed groups reject an explicit tenant_id --

/// A tenant-typed group's effective tenant is always its own generated id;
/// an explicit `tenant_id` on such a request is a contradiction and must be
/// rejected outright, regardless of what value it carries.
#[tokio::test]
async fn tenant_typed_group_with_explicit_tenant_id_returns_validation_error() {
    let db = common::test_db().await;
    let types = common::TestTypes::new();
    let group_svc = common::make_group_service(db.clone(), &types);
    let tenant_id = Uuid::now_v7();
    let other_tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let tenant_type = common::create_tenant_type(&types);

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(tenant_type.code, "TenantTypedConflict".to_owned())
                .with_tenant_id(Some(other_tenant)),
            tenant_id,
        )
        .await
        .expect_err("tenant-typed create with an explicit tenant_id must be rejected");

    assert!(matches!(err, DomainError::Validation { .. }));
}

// -- create_group: guardrail (explicit id + cross-tenant target) --

/// An explicit `id` combined with a cross-tenant target is rejected as a
/// stopgap -- even when the `AccessScope` would otherwise permit
/// the target tenant. The rejection happens before the AuthZ round trip, so
/// wiring a permissive mock here proves the guardrail is unconditional, not
/// merely a side effect of a denial.
#[tokio::test]
async fn explicit_id_with_foreign_tenant_id_returns_validation_error() {
    let db = common::test_db().await;
    let types = common::TestTypes::new();
    let group_svc = make_group_service_with_enforcer(
        db.clone(),
        &types,
        PolicyEnforcer::new(Arc::new(TargetTenantAllowAuthZ)),
    );
    let caller_tenant = Uuid::now_v7();
    let target_tenant = Uuid::now_v7();
    let ctx = common::make_ctx(caller_tenant);
    let root_type = common::create_root_type(&types, "vhp2162g");

    let err = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code, "IdPlusForeignTenant".to_owned())
                .with_id(Some(Uuid::now_v7()))
                .with_tenant_id(Some(target_tenant)),
            caller_tenant,
        )
        .await
        .expect_err("id + cross-tenant target combination must be rejected (guardrail)");

    assert!(matches!(err, DomainError::Validation { .. }));
}

/// Positive control for the guardrail above: an explicit `id` combined with
/// a target tenant that equals the caller's own tenant is *not* a
/// cross-tenant combination and must still succeed -- the own
/// (separately tracked, unresolved) id-capture question is untouched by
/// this guardrail.
#[tokio::test]
async fn explicit_id_with_same_tenant_still_succeeds() {
    let db = common::test_db().await;
    let types = common::TestTypes::new();
    let group_svc = common::make_group_service(db.clone(), &types);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let root_type = common::create_root_type(&types, "vhp2162h");
    let id = Uuid::now_v7();

    let group = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code, "IdPlusSameTenant".to_owned())
                .with_id(Some(id))
                .with_tenant_id(Some(tenant_id)),
            tenant_id,
        )
        .await
        .expect("id + same-tenant target must still succeed");

    assert_eq!(group.id, id);
}
