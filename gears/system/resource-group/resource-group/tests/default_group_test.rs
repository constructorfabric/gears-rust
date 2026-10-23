//! Cross-tenant ID reservation and authenticated default-group regressions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
    PolicyEnforcer,
    constraints::{Constraint, InPredicate, Predicate},
};
use resource_group::domain::{
    error::DomainError,
    group_service::{GroupService, QueryProfile},
    rg_service::RgService,
    seeding::{GroupSeedDef, seed_groups},
};
use resource_group::infra::storage::{group_repo::GroupRepository, type_repo::TypeRepository};
use resource_group_sdk::{
    CreateGroupRequest, EnsureDefaultGroupRequest, ResourceGroupClient, ResourceGroupDefaults,
    TENANT_RG_TYPE_PATH, default_group::default_group_id,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, pep_properties};
use uuid::Uuid;

fn request(code: &str) -> EnsureDefaultGroupRequest {
    EnsureDefaultGroupRequest {
        code: code.to_owned(),
        metadata: None,
    }
}

/// Both SDK surfaces use the same service enforcement. An attacker can choose
/// another creatable type, but that does not create a separate primary-key space.
#[tokio::test]
async fn ordinary_sdk_cannot_squat_default_id_with_any_type() {
    let db = common::test_db().await;
    let types = Arc::new(common::make_type_service(db.clone()));
    let groups = Arc::new(common::make_group_service(db.clone()));
    let sdk = RgService::new(
        types.clone(),
        groups.clone(),
        Arc::new(common::make_membership_service(db)),
    );
    let first = common::create_root_type(&types, "defaultid").await;
    let other = common::create_root_type(&types, "otherid").await;
    let attacker = common::make_ctx(Uuid::now_v7());
    let victim = common::make_ctx(Uuid::now_v7());
    let id = default_group_id(victim.subject_tenant_id());

    for code in [&first.code, &other.code] {
        for target in [
            None,
            Some(attacker.subject_tenant_id()),
            Some(victim.subject_tenant_id()),
        ] {
            let err = sdk
                .create_group(
                    &attacker,
                    CreateGroupRequest::new(code, "squat")
                        .with_id(Some(id))
                        .with_tenant_id(target),
                )
                .await
                .unwrap_err();
            assert!(matches!(err, CanonicalError::InvalidArgument { .. }));
        }
    }
    assert!(matches!(
        groups.get_group_unscoped(id).await,
        Err(DomainError::GroupNotFound { .. })
    ));
    let group = sdk
        .ensure_default_group(&victim, request(&first.code))
        .await
        .unwrap();
    assert_eq!(group.id, id);
    assert_eq!(group.hierarchy.tenant_id, victim.subject_tenant_id());

    // The ensure API has no target/ID parameter: an attacker ensuring its own
    // default obtains a different ID even when supplying the victim's type.
    let own = sdk
        .ensure_default_group(&attacker, request(&first.code))
        .await
        .unwrap();
    assert_ne!(own.id, id);
    assert_eq!(own.hierarchy.tenant_id, attacker.subject_tenant_id());
}

#[tokio::test]
async fn ensure_preserves_metadata_and_recreates_the_same_id_after_delete() {
    let db = common::test_db().await;
    let types = common::make_type_service(db.clone());
    let groups = common::make_group_service(db);
    let ty = common::create_root_type(&types, "ensure").await;
    let ctx = common::make_ctx(Uuid::now_v7());
    let mut req = request(&ty.code);
    req.metadata = Some(serde_json::json!({"default": true}));
    let created = groups
        .ensure_default_group(&ctx, req.clone())
        .await
        .unwrap();
    req.metadata = Some(serde_json::json!({"default": false}));
    let existing = groups.ensure_default_group(&ctx, req).await.unwrap();
    assert_eq!(created.id, existing.id);
    assert_eq!(created.metadata, existing.metadata);
    groups.delete_group(&ctx, created.id, false).await.unwrap();
    let recreated = groups
        .ensure_default_group(&ctx, request(&ty.code))
        .await
        .unwrap();
    assert_eq!(recreated.id, created.id);
}

/// Simulate rows predating the fix through the trusted configuration path.
/// No conflict variant may be silently adopted or overwritten by ensure.
#[tokio::test]
async fn ensure_rejects_foreign_tenant_wrong_type_name_and_parent() {
    for mismatch in ["tenant", "type", "name", "parent"] {
        let db = common::test_db().await;
        let types = common::make_type_service(db.clone());
        let groups = common::make_group_service(db);
        let ty = common::create_root_type(&types, "identity").await;
        let other = common::create_root_type(&types, "otheridentity").await;
        let victim = Uuid::now_v7();
        let owner = if mismatch == "tenant" {
            Uuid::now_v7()
        } else {
            victim
        };
        let owner_ctx = common::make_ctx(owner);
        let id = default_group_id(victim);
        let parent = if mismatch == "parent" {
            // A root-capable type may also accept this type as a parent.
            types
                .update_type_unscoped(
                    &ty.code,
                    resource_group_sdk::UpdateTypeRequest {
                        can_be_root: true,
                        allowed_parent_types: vec![other.code.clone()],
                        allowed_membership_types: vec![],
                        metadata_schema: None,
                    },
                )
                .await
                .unwrap();
            Some(
                common::create_root_group(&groups, &owner_ctx, &other.code, "parent", owner)
                    .await
                    .id,
            )
        } else {
            None
        };
        let req = CreateGroupRequest::new(
            if mismatch == "type" {
                &other.code
            } else {
                &ty.code
            },
            if mismatch == "name" {
                "renamed"
            } else {
                "default"
            },
        )
        .with_id(Some(id))
        .with_parent_id(parent);
        let original = groups.create_group_unscoped(req, owner).await.unwrap();
        let err = groups
            .ensure_default_group(&common::make_ctx(victim), request(&ty.code))
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::GroupAlreadyExists { .. }),
            "{mismatch}: {err:?}"
        );
        let unchanged = groups.get_group_unscoped(id).await.unwrap();
        assert_eq!(unchanged.hierarchy.tenant_id, original.hierarchy.tenant_id);
        assert_eq!(unchanged.hierarchy.parent_id, original.hierarchy.parent_id);
        assert_eq!(unchanged.code, original.code);
        assert_eq!(unchanged.name, original.name);
    }
}

#[tokio::test]
async fn ensure_requires_authorization_even_when_default_already_exists() {
    let db = common::test_db().await;
    let types = common::make_type_service(db.clone());
    let allowed = common::make_group_service(db.clone());
    let denied = common::make_group_service_deny(db);
    let ty = common::create_root_type(&types, "denieddefault").await;
    let ctx = common::make_ctx(Uuid::now_v7());
    for exists in [false, true] {
        if exists {
            allowed
                .ensure_default_group(&ctx, request(&ty.code))
                .await
                .unwrap();
        }
        let err = denied
            .ensure_default_group(&ctx, request(&ty.code))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::AccessDenied { .. }));
    }
}

/// A positive decision for a foreign tenant is not authorization for the home
/// tenant. This guards the deterministic path's scope check, not only the PDP's
/// decision bit.
struct ForeignScope(Uuid);
#[async_trait]
impl AuthZResolverApi for ForeignScope {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [self.0],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

#[tokio::test]
async fn ensure_rejects_a_grant_for_a_different_tenant() {
    let db = common::test_db().await;
    let types = common::make_type_service(db.clone());
    let ty = common::create_root_type(&types, "foreignscope").await;
    let groups = GroupService::new(
        db,
        QueryProfile::default(),
        PolicyEnforcer::new(Arc::new(ForeignScope(Uuid::now_v7()))),
        Arc::new(GroupRepository),
        Arc::new(TypeRepository),
        common::make_types_registry(),
    );
    let ctx = common::make_ctx(Uuid::now_v7());
    assert!(matches!(
        groups.ensure_default_group(&ctx, request(&ty.code)).await,
        Err(DomainError::TenantNotFound { .. })
    ));
    assert!(matches!(
        groups
            .get_group_unscoped(default_group_id(ctx.subject_tenant_id()))
            .await,
        Err(DomainError::GroupNotFound { .. })
    ));
}

#[tokio::test]
async fn ensure_cannot_create_a_new_tenant_scope() {
    let groups = common::make_group_service(common::test_db().await);
    let ctx = common::make_ctx(Uuid::now_v7());
    assert!(matches!(
        groups
            .ensure_default_group(&ctx, request(TENANT_RG_TYPE_PATH))
            .await,
        Err(DomainError::Validation { .. })
    ));
}

#[tokio::test]
async fn trusted_seeding_keeps_fixed_ids_but_rejects_foreign_existing_rows() {
    let db = common::test_db().await;
    let types = common::make_type_service(db.clone());
    let groups = common::make_group_service(db);
    let ty = common::create_root_type(&types, "seedownership").await;
    let mut seed = GroupSeedDef {
        id: Uuid::now_v7(),
        code: ty.code,
        name: "seed".to_owned(),
        parent_id: None,
        metadata: None,
        tenant_id: Uuid::now_v7(),
    };
    assert_eq!(
        seed_groups(&groups, &[seed.clone()]).await.unwrap().created,
        1
    );
    assert_eq!(
        seed_groups(&groups, &[seed.clone()])
            .await
            .unwrap()
            .unchanged,
        1
    );
    seed.tenant_id = Uuid::now_v7();
    assert!(matches!(
        seed_groups(&groups, &[seed]).await,
        Err(DomainError::GroupAlreadyExists { .. })
    ));
}

/// Inject a denial or backend failure at either ensure authorization point.
struct EnsureAuthZFailure {
    tenant: Uuid,
    action: &'static str,
    outage: bool,
}

#[async_trait]
impl AuthZResolverApi for EnsureAuthZFailure {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let fail = request.action.name == self.action;
        if fail && self.outage {
            return Err(types_registry_sdk::testing::internal("PDP unavailable"));
        }
        Ok(EvaluationResponse {
            decision: !fail,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [self.tenant],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

#[tokio::test]
async fn ensure_propagates_create_and_confirm_denials_and_outages() {
    for action in ["create", "get"] {
        for outage in [false, true] {
            let db = common::test_db().await;
            let types = common::make_type_service(db.clone());
            let ty = common::create_root_type(&types, "ensureauthfailure").await;
            let ctx = common::make_ctx(Uuid::now_v7());
            let tenant = ctx.subject_tenant_id();
            let groups = GroupService::new(
                db,
                QueryProfile::default(),
                PolicyEnforcer::new(Arc::new(EnsureAuthZFailure {
                    tenant,
                    action,
                    outage,
                })),
                Arc::new(GroupRepository),
                Arc::new(TypeRepository),
                common::make_types_registry(),
            );
            if action == "get" {
                groups
                    .create_group_unscoped(
                        CreateGroupRequest::new(&ty.code, "default")
                            .with_id(Some(default_group_id(tenant))),
                        tenant,
                    )
                    .await
                    .unwrap();
            }
            let err = groups
                .ensure_default_group(&ctx, request(&ty.code))
                .await
                .unwrap_err();
            if outage {
                assert!(
                    matches!(err, DomainError::InternalError),
                    "{action}: {err:?}"
                );
            } else {
                assert!(
                    matches!(err, DomainError::AccessDenied { .. }),
                    "{action}: {err:?}"
                );
            }
            if action == "create" {
                assert!(matches!(
                    groups.get_group_unscoped(default_group_id(tenant)).await,
                    Err(DomainError::GroupNotFound { .. })
                ));
            }
        }
    }
}
