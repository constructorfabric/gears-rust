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
//! - a tenant-typed group (`code` starting with `TENANT_GROUP_TYPE_ID`) with
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

use authz_resolver_sdk::test_support::EmptyScopePdp;
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

use common::CreateTypeRequest;
use resource_group::domain::error::DomainError;
use resource_group::domain::group_service::{GroupService, QueryProfile};
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group_sdk::{CreateGroupRequest, TENANT_GROUP_TYPE_ID};

/// Build a `GroupService` wired with a caller-supplied enforcer, otherwise
/// identical to `common::make_group_service`.
fn make_group_service_with_enforcer(
    db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    enforcer: PolicyEnforcer,
) -> GroupService<GroupRepository> {
    GroupService::new(
        db,
        QueryProfile::default(),
        enforcer,
        Arc::new(GroupRepository),
        common::make_types_catalog(),
    )
}

/// Build a unique tenant-type code (mirrors
/// `group_service_test.rs::unique_tenant_type_code`): a code starting with
/// `TENANT_GROUP_TYPE_ID` classifies the group as tenant-typed.
fn unique_tenant_type_code() -> String {
    format!(
        "{}x.test.tn.i{}.v1~",
        TENANT_GROUP_TYPE_ID,
        Uuid::now_v7().as_simple()
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

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
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

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

// -- create_group: omitted / same-tenant tenant_id (backward compatibility) --

/// Omitted `tenant_id` must be byte-for-byte the previous behavior:
/// target tenant == the caller's own tenant, no extra AuthZ round trip
/// beyond the one `create_group` always performed.
#[tokio::test]
async fn tenant_id_omitted_uses_caller_tenant() {
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let root_type = common::create_root_type(&type_svc, "vhp2162a").await;

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
    let type_svc = common::make_type_service(db.clone());
    // `common::make_group_service` wires `AllowAllAuthZ`, which clamps the
    // returned scope to the caller's own tenant -- if the extra
    // check ran here, it would still pass (target == caller), but this test
    // pins that the common "explicit but same tenant" case behaves
    // identically to omission, not merely that it happens to also pass.
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let root_type = common::create_root_type(&type_svc, "vhp2162b").await;

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
    let type_svc = common::make_type_service(db.clone());
    let group_svc = make_group_service_with_enforcer(
        db.clone(),
        PolicyEnforcer::new(Arc::new(TargetTenantAllowAuthZ)),
    );
    let caller_tenant = Uuid::now_v7();
    let target_tenant = Uuid::now_v7();
    let ctx = common::make_ctx(caller_tenant);
    let root_type = common::create_root_type(&type_svc, "vhp2162c").await;

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
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let caller_tenant = Uuid::now_v7();
    let target_tenant = Uuid::now_v7();
    let ctx = common::make_ctx(caller_tenant);
    let root_type = common::create_root_type(&type_svc, "vhp2162d").await;

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
    let type_svc = common::make_type_service(db.clone());
    let caller_tenant = Uuid::now_v7();
    let target_tenant = Uuid::now_v7();
    let group_svc = make_group_service_with_enforcer(
        db.clone(),
        PolicyEnforcer::new(Arc::new(InTenantSubtreeOnlyAuthZ {
            root_tenant_id: target_tenant,
        })),
    );
    let ctx = common::make_ctx(caller_tenant);
    let root_type = common::create_root_type(&type_svc, "vhp2162e").await;

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
    let type_svc = common::make_type_service(db.clone());
    let group_svc = make_group_service_with_enforcer(
        db.clone(),
        PolicyEnforcer::new(Arc::new(TargetTenantAllowAuthZ)),
    );
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_a);

    let root_type = common::create_root_type(&type_svc, "vhp2162f").await;
    let child_type =
        common::create_child_type(&type_svc, "vhp2162f-child", &[&root_type.code], &[]).await;

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
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let other_tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let tenant_type = type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: unique_tenant_type_code(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create tenant type");

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
    let type_svc = common::make_type_service(db.clone());
    let group_svc = make_group_service_with_enforcer(
        db.clone(),
        PolicyEnforcer::new(Arc::new(TargetTenantAllowAuthZ)),
    );
    let caller_tenant = Uuid::now_v7();
    let target_tenant = Uuid::now_v7();
    let ctx = common::make_ctx(caller_tenant);
    let root_type = common::create_root_type(&type_svc, "vhp2162g").await;

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
    let type_svc = common::make_type_service(db.clone());
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let root_type = common::create_root_type(&type_svc, "vhp2162h").await;
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

/// Empty permits must not become writes, including on the own-tenant shortcut.
#[tokio::test]
async fn empty_scope_never_authorizes_group_creation() {
    use resource_group::domain::repo::GroupRepositoryTrait;
    struct EmptyBranchAuthZ {
        target: Uuid,
        live_tenant: Option<Uuid>,
    }
    #[async_trait]
    impl AuthZResolverApi for EmptyBranchAuthZ {
        async fn evaluate(
            &self,
            _ctx: PlatformSecurityContext,
            _request: EvaluationRequest,
        ) -> Result<EvaluationResponse, CanonicalError> {
            let mut constraints = vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [self.target],
                    )),
                    Predicate::In(InPredicate::new(
                        pep_properties::RESOURCE_ID,
                        Vec::<Uuid>::new(),
                    )),
                ],
            }];
            if let Some(tenant) = self.live_tenant {
                constraints.push(Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tenant],
                    ))],
                });
            }
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints,
                    deny_reason: None,
                },
            })
        }

        async fn evaluate_batch(
            &self,
            ctx: PlatformSecurityContext,
            req: authz_resolver_sdk::EvaluationBatchRequest,
        ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
            authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
        }
    }
    let db = common::test_db().await;
    let type_svc = common::make_type_service(db.clone());
    let root_type = common::create_root_type(&type_svc, "emptyscope").await;
    let caller = Uuid::from_u128(1);
    let foreign = Uuid::from_u128(2);
    let ctx = common::make_ctx(caller);
    let mut expected_rows = 0;
    for (target, live_tenant) in [
        (caller, None),
        (foreign, None),
        (foreign, Some(caller)),
        (caller, Some(caller)),
        (foreign, Some(foreign)),
    ] {
        let svc = make_group_service_with_enforcer(
            db.clone(),
            PolicyEnforcer::new(Arc::new(EmptyBranchAuthZ {
                target,
                live_tenant,
            })),
        );
        let result = svc
            .create_group(
                &ctx,
                CreateGroupRequest::new(root_type.code.clone(), "EmptyScope".to_owned())
                    .with_tenant_id(Some(target)),
                caller,
            )
            .await;
        if live_tenant == Some(target) {
            let group = result.expect("a live OR alternative authorizes creation");
            assert_eq!(group.hierarchy.tenant_id, target);
            expected_rows += 1;
        } else if live_tenant.is_none() {
            // All branches empty: the legacy PEP refuses before any tenant gate.
            assert!(
                matches!(result, Err(DomainError::AccessDenied { .. })),
                "{result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(DomainError::TenantNotFound { .. })),
                "{result:?}"
            );
        }
        let stored = GroupRepository
            .list_groups(
                &db.conn().expect("connection"),
                &toolkit_security::AccessScope::allow_all(),
                &toolkit_odata::ODataQuery::new(),
            )
            .await
            .expect("inspect groups after authorization");
        assert_eq!(
            stored.items.len(),
            expected_rows,
            "a refused create writes no group"
        );
    }
}

#[tokio::test]
async fn empty_scope_cannot_create_memberships() {
    use resource_group::domain::membership_service::MembershipService;
    use resource_group::infra::storage::membership_repo::MembershipRepository;
    let db = common::test_db().await;
    let tenant = Uuid::from_u128(1);
    let ctx = common::make_ctx(tenant);
    let seed_types = common::make_type_service(db.clone());
    let member_type = common::create_root_type(&seed_types, "emptymember").await;
    let group_type = seed_types
        .create_type_unscoped(CreateTypeRequest {
            code: format!(
                "{}x.test.emptygroup.i{}.v1~",
                toolkit_gts::gts_id!("cf.core.rg.group.v1~"),
                Uuid::now_v7().as_simple()
            ),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("seed membership-compatible group type");
    let group = common::create_root_group(
        &common::make_group_service(db.clone()),
        &ctx,
        &group_type.code,
        "Seed",
        tenant,
    )
    .await;
    let seed_memberships = common::make_membership_service(db.clone());
    seed_memberships
        .add_membership(&ctx, group.id, &member_type.code, "seed")
        .await
        .expect("seed membership");
    for live in [false, true] {
        let pep = PolicyEnforcer::new(Arc::new(EmptyScopePdp { live }));
        let memberships = MembershipService::new(
            db.clone(),
            pep,
            Arc::new(GroupRepository),
            common::make_types_catalog(),
            Arc::new(MembershipRepository),
        );
        let added = memberships
            .add_membership(&ctx, group.id, &member_type.code, "new")
            .await;
        assert_eq!(added.is_ok(), live);
        let removed = memberships
            .remove_membership(&ctx, group.id, &member_type.code, "seed")
            .await;
        assert_eq!(removed.is_ok(), live);
        if !live {
            assert!(matches!(added, Err(DomainError::AccessDenied { .. })));
            assert!(matches!(removed, Err(DomainError::AccessDenied { .. })));
        }
        let page = memberships
            .list_memberships(&ctx, &toolkit_odata::ODataQuery::new())
            .await
            .expect("list");
        assert_eq!(page.items.len(), usize::from(live));
        let stored = seed_memberships
            .list_memberships_unscoped(&toolkit_odata::ODataQuery::new())
            .await
            .expect("inspect memberships");
        assert_eq!(stored.items.len(), 1);
        assert_eq!(
            stored.items[0].resource_id,
            if live { "new" } else { "seed" }
        );
    }
}

#[tokio::test]
async fn empty_scope_lists_succeed_and_still_validate_user_filters() {
    use resource_group::domain::membership_service::MembershipService;
    use resource_group::infra::storage::membership_repo::MembershipRepository;
    use toolkit_odata::{ODataQuery, parse_filter_string};
    let db = common::test_db().await;
    let tenant = Uuid::from_u128(1);
    let ctx = common::make_ctx(tenant);
    let seed_types = common::make_type_service(db.clone());
    let member_type = common::create_root_type(&seed_types, "emptylist").await;
    let group_type = seed_types
        .create_type_unscoped(CreateTypeRequest {
            code: format!(
                "{}x.test.emptylistgroup.i{}.v1~",
                toolkit_gts::gts_id!("cf.core.rg.group.v1~"),
                Uuid::now_v7().as_simple()
            ),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("seed membership-compatible group type");
    let group = common::create_root_group(
        &common::make_group_service(db.clone()),
        &ctx,
        &group_type.code,
        "Seed",
        tenant,
    )
    .await;
    common::make_membership_service(db.clone())
        .add_membership(&ctx, group.id, &member_type.code, "seed")
        .await
        .expect("seed membership");
    let query = |raw: &str| {
        ODataQuery::new().with_filter(parse_filter_string(raw).expect("parse filter").into_expr())
    };
    for live in [false, true] {
        let pep = PolicyEnforcer::new(Arc::new(EmptyScopePdp { live }));
        let members = MembershipService::new(
            db.clone(),
            pep,
            Arc::new(GroupRepository),
            common::make_types_catalog(),
            Arc::new(MembershipRepository),
        );
        // The seeded membership matches the user filter, so only the empty
        // scope can hide it.
        let seeded = members
            .list_memberships(&ctx, &query("resource_id eq 'seed'"))
            .await
            .expect("valid user filter");
        assert_eq!(seeded.items.len(), usize::from(live), "live={live}");
        let invalid_filter = query("unknown_field eq 'x'");
        assert!(matches!(
            members.list_memberships(&ctx, &invalid_filter).await,
            Err(DomainError::Validation { .. })
        ));
    }
}

#[tokio::test]
async fn list_groups_empty_scope_preserves_live_or() {
    let db = common::test_db().await;
    let types = common::make_type_service(db.clone());
    let group_type = common::create_root_type(&types, "emptygrouplist").await;
    let tenant = Uuid::new_v4();
    let ctx = common::make_ctx(tenant);
    let seed = common::make_group_service(db.clone());
    let own = common::create_root_group(&seed, &ctx, &group_type.code, "Own", tenant).await;
    let other = Uuid::new_v4();
    common::create_root_group(
        &seed,
        &common::make_ctx(other),
        &group_type.code,
        "Other",
        other,
    )
    .await;
    for live in [false, true] {
        let svc = make_group_service_with_enforcer(
            db.clone(),
            PolicyEnforcer::new(Arc::new(EmptyScopePdp { live })),
        );
        let page = svc
            .list_groups(&ctx, &toolkit_odata::ODataQuery::new())
            .await
            .expect("permitted LIST succeeds even when its scope is empty");
        assert_eq!(page.items.len(), usize::from(live));
        if live {
            assert_eq!(page.items[0].id, own.id);
        }
    }
}
