#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;

use super::*;
use crate::test_providers::{AllowShape, FakePdp};

fn user(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xabc))
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap()
}

#[test]
fn the_audited_operations_are_exactly_the_privileged_actions() {
    for operation in Operation::ALL {
        let (_, action) = operation.check();
        let privileged = matches!(
            action,
            actions::RETRY
                | actions::RELEASE
                | actions::EXECUTE
                | actions::RESOLVE
                | actions::APPROVE
                | actions::PLACE
                | actions::ACCEPT_DELAY_ONLY
                | actions::WRITE
        );
        assert_eq!(operation.audited(), privileged, "{operation:?}");
    }
}

#[test]
fn every_operation_has_a_unique_id_in_the_rating_namespace() {
    let ids: HashSet<&str> = Operation::ALL.iter().map(|o| o.operation_id()).collect();
    assert_eq!(ids.len(), Operation::ALL.len());
    assert!(ids.iter().all(|id| id.starts_with("bss_rating.")));
}

#[test]
fn every_operation_checks_a_registered_label() {
    for operation in Operation::ALL {
        let (resource, _) = operation.check();
        assert!(labels::ALL.contains(&resource.name()), "{operation:?}");
    }
}

#[test]
fn the_inbox_release_is_usage_release() {
    let (resource, action) = Operation::InboxRelease.check();
    assert_eq!(resource.name(), labels::USAGE);
    assert_eq!(action, actions::RELEASE);
    assert!(Operation::InboxRelease.audited());
}

#[test]
fn label_schemas_cover_every_label() {
    let schemas = authz_label_type_schemas();
    assert_eq!(schemas.len(), labels::ALL.len());
    for (schema, label) in schemas.iter().zip(labels::ALL) {
        assert_eq!(schema["$id"], format!("gts://{label}"));
    }
}

#[tokio::test]
async fn the_pdp_scope_is_the_callers_tenant() {
    let pdp = FakePdp::new();
    let tenant = Uuid::from_u128(1);
    let scope = authorize(
        &pdp.enforcer(),
        &user(tenant),
        Operation::InboxRelease,
        None,
    )
    .await
    .unwrap();
    assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant));
    assert!(!scope.contains_uuid(pep_properties::OWNER_TENANT_ID, Uuid::from_u128(2)));
}

#[tokio::test]
async fn deny_by_default() {
    let pdp = FakePdp::new();
    pdp.deny(labels::USAGE, actions::RELEASE);
    assert!(matches!(
        authorize(
            &pdp.enforcer(),
            &user(Uuid::from_u128(1)),
            Operation::InboxRelease,
            None
        )
        .await,
        Err(DomainError::PermissionDenied(_))
    ));
}

#[tokio::test]
async fn a_pdp_outage_refuses() {
    let pdp = FakePdp::new();
    pdp.set_unavailable(true);
    assert!(matches!(
        authorize(
            &pdp.enforcer(),
            &user(Uuid::from_u128(1)),
            Operation::InboxRelease,
            None
        )
        .await,
        Err(DomainError::AuthzUnavailable(..))
    ));
}

#[tokio::test]
async fn a_write_to_another_tenant_is_denied() {
    let pdp = FakePdp::new();
    assert!(matches!(
        authorize(
            &pdp.enforcer(),
            &user(Uuid::from_u128(1)),
            Operation::ReratingsCreate,
            Some(Uuid::from_u128(2))
        )
        .await,
        Err(DomainError::PermissionDenied(_))
    ));
}

#[test]
fn a_selector_only_narrows_the_scope() {
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let scope = AccessScope::for_tenants(vec![a, b]);
    let narrowed = narrow_to_tenant(&scope, a, SelectorRead::Collection).unwrap();
    assert!(narrowed.contains_uuid(pep_properties::OWNER_TENANT_ID, a));
    assert!(!narrowed.contains_uuid(pep_properties::OWNER_TENANT_ID, b));
}

#[test]
fn a_selector_outside_the_scope_is_403_on_collections_and_404_on_objects() {
    let scope = AccessScope::for_tenant(Uuid::from_u128(1));
    let outside = Uuid::from_u128(2);
    assert!(matches!(
        narrow_to_tenant(&scope, outside, SelectorRead::Collection),
        Err(DomainError::PermissionDenied(_))
    ));
    assert!(matches!(
        narrow_to_tenant(&scope, outside, SelectorRead::Object),
        Err(DomainError::NotFound { .. })
    ));
}

#[tokio::test]
async fn the_service_grant_is_per_tenant() {
    let pdp = FakePdp::new();
    let inside = Uuid::from_u128(1);
    let outside = Uuid::from_u128(2);
    pdp.put_outside_grant(outside);
    let grant = ServiceGrant::new(pdp.enforcer());
    let scope = grant.scope_for(inside).await.unwrap();
    assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, inside));
    assert_eq!(
        grant.scope_for(outside).await,
        Err(DomainError::OutsideGrant { tenant_id: outside })
    );
}

#[tokio::test]
async fn an_allow_without_constraints_fails_closed() {
    let pdp = FakePdp::new();
    pdp.set_allow_shape(AllowShape::NoConstraints);
    let tenant = Uuid::from_u128(1);
    let refused = authorize(
        &pdp.enforcer(),
        &user(tenant),
        Operation::InboxRelease,
        None,
    )
    .await;
    assert_eq!(
        refused,
        Err(DomainError::PermissionDenied(
            CONSTRAINT_COMPILATION_FAILED.to_owned()
        ))
    );
}

#[tokio::test]
async fn an_allow_whose_constraints_do_not_compile_fails_closed() {
    let pdp = FakePdp::new();
    pdp.set_allow_shape(AllowShape::UnknownProperty);
    let tenant = Uuid::from_u128(1);
    let refused = authorize(
        &pdp.enforcer(),
        &user(tenant),
        Operation::InboxRelease,
        None,
    )
    .await;
    assert_eq!(
        refused,
        Err(DomainError::PermissionDenied(
            CONSTRAINT_COMPILATION_FAILED.to_owned()
        ))
    );
}

#[test]
fn an_unconstrained_scope_is_never_narrowed_into_a_grant() {
    assert!(matches!(
        narrow_to_tenant(
            &AccessScope::allow_all(),
            Uuid::from_u128(1),
            SelectorRead::Collection
        ),
        Err(DomainError::PermissionDenied(_))
    ));
}

#[test]
fn narrowing_keeps_only_the_access_paths_that_admit_the_tenant() {
    let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let owner = pep_properties::OWNER_TENANT_ID;
    let scope = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![ScopeFilter::in_uuids(owner, vec![b])]),
        ScopeConstraint::new(vec![
            ScopeFilter::in_uuids(owner, vec![a, b]),
            ScopeFilter::eq("kind", "fact"),
        ]),
    ]);
    let narrowed = narrow_to_tenant(&scope, a, SelectorRead::Collection).unwrap();
    // The path that admits only `b` is dropped; the other keeps its other filters, with its
    // tenant filter intersected to `a`.
    assert_eq!(
        narrowed.constraints(),
        &[ScopeConstraint::new(vec![
            ScopeFilter::in_uuids(owner, vec![a]),
            ScopeFilter::eq("kind", "fact"),
        ])]
    );
}

#[test]
fn a_tenant_subtree_filter_stays_beside_the_selector() {
    let (a, root) = (Uuid::from_u128(1), Uuid::from_u128(9));
    let owner = pep_properties::OWNER_TENANT_ID;
    let subtree = ScopeFilter::in_tenant_subtree(owner, root, true, Vec::new());
    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(owner, vec![a]),
        subtree.clone(),
    ])]);
    let narrowed = narrow_to_tenant(&scope, a, SelectorRead::Object).unwrap();
    // The subtree cannot be decided in memory, so the database ANDs it with the selector.
    assert_eq!(
        narrowed.constraints(),
        &[ScopeConstraint::new(vec![
            ScopeFilter::in_uuids(owner, vec![a]),
            subtree,
        ])]
    );
}

#[tokio::test]
async fn the_service_grant_asks_the_pdp_as_ratings_identity_in_the_tenant() {
    let pdp = FakePdp::new();
    let tenant = Uuid::from_u128(7);
    ServiceGrant::new(pdp.enforcer())
        .scope_for(tenant)
        .await
        .unwrap();
    let asked = pdp.subjects();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].id, SERVICE_SUBJECT_ID);
    assert_eq!(asked[0].subject_type.as_deref(), Some(SERVICE_SUBJECT_TYPE));
    assert_eq!(asked[0].tenant_id, Some(tenant));
}

#[test]
fn every_label_is_the_resource_type_of_its_sdk_error() {
    use bss_rating_sdk::errors::{
        EvaluationResource, FinalizationPolicyResource, PlatformFinalizationPolicyResource,
        RerateResource, ResultResource, RetentionHoldResource, SourceLossResource, UsageResource,
    };
    let sdk = [
        (
            labels::USAGE,
            UsageResource::not_found("x").with_resource("x").create(),
        ),
        (
            labels::RESULT,
            ResultResource::not_found("x").with_resource("x").create(),
        ),
        (
            labels::RERATE,
            RerateResource::not_found("x").with_resource("x").create(),
        ),
        (
            labels::EVALUATION,
            EvaluationResource::not_found("x")
                .with_resource("x")
                .create(),
        ),
        (
            labels::FINALIZATION_POLICY,
            FinalizationPolicyResource::not_found("x")
                .with_resource("x")
                .create(),
        ),
        (
            labels::PLATFORM_FINALIZATION_POLICY,
            PlatformFinalizationPolicyResource::not_found("x")
                .with_resource("x")
                .create(),
        ),
        (
            labels::SOURCE_LOSS,
            SourceLossResource::not_found("x")
                .with_resource("x")
                .create(),
        ),
        (
            labels::RETENTION_HOLD,
            RetentionHoldResource::not_found("x")
                .with_resource("x")
                .create(),
        ),
    ];
    assert_eq!(sdk.len(), labels::ALL.len());
    for (label, error) in sdk {
        assert_eq!(error.resource_type(), Some(label), "{label}");
    }
}

#[test]
fn the_service_identity_acts_in_the_work_items_tenant() {
    let tenant = Uuid::from_u128(5);
    let ctx = service_context(tenant).unwrap();
    assert_eq!(ctx.subject_tenant_id(), tenant);
    assert_eq!(ctx.subject_id(), SERVICE_SUBJECT_ID);
    assert_eq!(ctx.subject_type(), Some(SERVICE_SUBJECT_TYPE));
}
