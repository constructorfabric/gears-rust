#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;

use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, SecurityContext, pep_properties,
};
use uuid::Uuid;

use super::{
    CONSTRAINT_COMPILATION_FAILED, Operation, SERVICE_SUBJECT_ID, SERVICE_SUBJECT_TYPE,
    SelectorRead, ServiceGrant, actions, admits_write, authorize, authz_label_type_schemas, labels,
    narrow_to_tenant, require_write,
};
use crate::domain::error::DomainError;
use crate::domain::ports::TenantGrant;
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
    for &operation in Operation::ALL {
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
    for &operation in Operation::ALL {
        let (resource, _) = operation.check();
        assert!(labels::ALL.contains(&resource.name()), "{operation:?}");
    }
}

#[test]
fn every_operation_asks_the_pdp_its_designed_resource_and_action() {
    // DESIGN §4.8, row by row: a wrong pair is a wrong authorization decision, so each is pinned.
    use Operation as O;
    let matrix = [
        (O::RunsGet, labels::RESULT, actions::READ),
        (O::WindowsList, labels::RESULT, actions::READ),
        (O::DeliveriesSince, labels::RESULT, actions::READ),
        (O::SnapshotsGet, labels::RESULT, actions::READ),
        (O::ExceptionsList, labels::USAGE, actions::READ),
        (O::ExceptionsRetry, labels::USAGE, actions::RETRY),
        (O::InboxRelease, labels::USAGE, actions::RELEASE),
        (O::ReratingsCreate, labels::RERATE, actions::EXECUTE),
        (O::ReratingsGet, labels::RERATE, actions::READ),
        (O::ReratingsCancel, labels::RERATE, actions::EXECUTE),
        (O::OrderEvaluate, labels::EVALUATION, actions::EVALUATE),
        (
            O::FinalizationPoliciesList,
            labels::FINALIZATION_POLICY,
            actions::READ,
        ),
        (
            O::FinalizationPoliciesCreate,
            labels::FINALIZATION_POLICY,
            actions::WRITE,
        ),
        (
            O::FinalizationPoliciesAcceptDelayOnly,
            labels::FINALIZATION_POLICY,
            actions::ACCEPT_DELAY_ONLY,
        ),
        (
            O::PlatformFinalizationPoliciesRead,
            labels::PLATFORM_FINALIZATION_POLICY,
            actions::READ,
        ),
        (
            O::PlatformFinalizationPoliciesCreate,
            labels::PLATFORM_FINALIZATION_POLICY,
            actions::WRITE,
        ),
        (
            O::PlatformFinalizationPoliciesAcceptDelayOnly,
            labels::PLATFORM_FINALIZATION_POLICY,
            actions::ACCEPT_DELAY_ONLY,
        ),
        (O::SourceLossesList, labels::USAGE, actions::READ),
        (
            O::SourceLossesSubmitReconciliation,
            labels::SOURCE_LOSS,
            actions::RESOLVE,
        ),
        (
            O::SourceLossesResolve,
            labels::SOURCE_LOSS,
            actions::RESOLVE,
        ),
        (
            O::SourceLossesApprove,
            labels::SOURCE_LOSS,
            actions::APPROVE,
        ),
        (O::SourceLossesReject, labels::SOURCE_LOSS, actions::APPROVE),
        (O::RetentionHoldsList, labels::RETENTION_HOLD, actions::READ),
        (
            O::RetentionHoldsCreate,
            labels::RETENTION_HOLD,
            actions::PLACE,
        ),
        (
            O::RetentionHoldsRelease,
            labels::RETENTION_HOLD,
            actions::RELEASE,
        ),
        (O::ServiceWork, labels::USAGE, actions::READ),
    ];
    assert_eq!(
        matrix.len(),
        Operation::ALL.len(),
        "every operation is pinned"
    );
    for (operation, label, action) in matrix {
        let (resource, asked) = operation.check();
        assert_eq!((resource.name(), asked), (label, action), "{operation:?}");
    }
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
fn a_subtree_grant_narrows_for_reads_and_leaves_membership_to_the_database() {
    let (a, root) = (Uuid::from_u128(1), Uuid::from_u128(9));
    let owner = pep_properties::OWNER_TENANT_ID;
    let subtree = ScopeFilter::in_tenant_subtree(owner, root, true, Vec::new());
    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![subtree.clone()])]);
    for read in [SelectorRead::Collection, SelectorRead::Object] {
        let narrowed = narrow_to_tenant(&scope, a, read).unwrap();
        assert_eq!(
            narrowed.constraints(),
            &[ScopeConstraint::new(vec![
                ScopeFilter::in_uuids(owner, vec![a]),
                subtree.clone(),
            ])]
        );
    }
}

#[test]
fn a_write_needs_a_path_whose_tenant_filters_all_list_the_tenant() {
    let (a, b, root) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(9));
    let owner = pep_properties::OWNER_TENANT_ID;
    let list = |tenants: Vec<Uuid>| ScopeFilter::in_uuids(owner, tenants);
    let subtree = ScopeFilter::in_tenant_subtree(owner, root, true, Vec::new());
    let path = |filters: Vec<ScopeFilter>| {
        AccessScope::from_constraints(vec![ScopeConstraint::new(filters)])
    };
    let cases = [
        (
            "a list holding the tenant",
            path(vec![list(vec![a, b])]),
            true,
        ),
        (
            "a list beside another property, left to SecureORM",
            path(vec![list(vec![a]), ScopeFilter::eq("kind", "fact")]),
            true,
        ),
        ("a list of another tenant", path(vec![list(vec![b])]), false),
        ("a subtree alone", path(vec![subtree.clone()]), false),
        (
            "a list beside a subtree",
            path(vec![list(vec![a]), subtree]),
            false,
        ),
        (
            "no tenant filter",
            path(vec![ScopeFilter::eq("kind", "fact")]),
            false,
        ),
    ];
    for (case, scope, admitted) in cases {
        assert_eq!(admits_write(&scope, a), admitted, "{case}");
        assert_eq!(
            require_write(&scope, a, Operation::InboxRelease).is_ok(),
            admitted,
            "{case}"
        );
    }
    // A second path that admits the tenant is enough.
    let either = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![list(vec![b])]),
        ScopeConstraint::new(vec![list(vec![a])]),
    ]);
    assert!(admits_write(&either, a));
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
