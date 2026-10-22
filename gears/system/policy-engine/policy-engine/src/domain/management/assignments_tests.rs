#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Assignment surface tests against in-memory `SQLite` with every migration
//! applied, the fake tenant tree and the fake PDP.

use policy_engine_sdk::gts::permissions::actions;
use policy_engine_sdk::management::{AssignmentSpec, ManagementError};
use time::OffsetDateTime;
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

use super::*;
use crate::domain::management::error::reason_of;
use crate::domain::management::test_support::governance::{
    ALICE, Harness, ROOT, TENANT_A, TENANT_A1, TENANT_B, TENANT_S, TENANT_S1, UNKNOWN_TENANT,
    alice, bob, harness, sam,
};
use crate::domain::management::test_support::{PdpMode, ctx};
use crate::domain::model::BundleId;
use crate::infra::storage::content_repo::OrmAssignmentRepository;

fn is_boundary(err: &ManagementError) {
    assert_eq!(err.category().http_status(), 403, "{err:?}");
    assert_eq!(reason_of(err).as_deref(), Some("TENANT_BOUNDARY"));
}

/// An assignment stored directly, as one made before its tenant raised a
/// barrier would be.
async fn stored_at(h: &Harness, bundle_id: BundleId, tenant: Uuid) -> Assignment {
    let conn = h.db.conn().unwrap();
    let at = OffsetDateTime::now_utc();
    OrmAssignmentRepository
        .insert(
            &conn,
            &AccessScope::allow_all(),
            &Assignment {
                id: AssignmentId(Uuid::new_v4()),
                bundle_id,
                tenant_id: tenant,
                owner_tenant_id: TENANT_A,
                enforce: true,
                created_at: at,
                created_by: ALICE,
                updated_at: at,
                updated_by: ALICE,
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn lifecycle_assign_update_unassign() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;

    let spec = AssignmentSpec::new(bundle.id, TENANT_A1);
    let created = h.service.assign(&a, spec.clone()).await.unwrap();
    assert_eq!(created.tenant_id, TENANT_A1);
    assert_eq!(created.owner_tenant_id, TENANT_A);
    assert!(created.enforce, "enforcing by default");

    // A second assignment of the bundle to the tenant.
    let exists = h.service.assign(&a, spec).await.unwrap_err();
    assert_eq!(exists.category().http_status(), 409);
    assert_eq!(reason_of(&exists).as_deref(), Some("ASSIGNMENT_EXISTS"));

    // Read back, then switch to shadow mode.
    assert_eq!(
        h.service.get_assignment(&a, created.id).await.unwrap(),
        created
    );
    let shadow = h
        .service
        .update_assignment(&a, created.id, false)
        .await
        .unwrap();
    assert!(!shadow.enforce);
    assert!(shadow.updated_at >= created.updated_at);
    assert_eq!(shadow.created_at, created.created_at);
    assert!(
        h.service
            .update_assignment(&a, created.id, true)
            .await
            .unwrap()
            .enforce
    );

    // A bundle can be assigned shadow from the start.
    let other = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A).shadow())
        .await
        .unwrap();
    assert!(!other.enforce);

    // Withdraw; withdrawing again (or a missing assignment) succeeds.
    h.service.unassign(&a, created.id).await.unwrap();
    assert_eq!(
        h.service
            .get_assignment(&a, created.id)
            .await
            .unwrap_err()
            .category()
            .http_status(),
        404
    );
    h.service.unassign(&a, created.id).await.unwrap();
    h.service
        .unassign(&a, AssignmentId(Uuid::new_v4()))
        .await
        .unwrap();
}

#[tokio::test]
async fn assign_checks_bundle_and_capability() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let missing = h
        .service
        .assign(&a, AssignmentSpec::new(BundleId(Uuid::new_v4()), TENANT_A))
        .await
        .unwrap_err();
    assert_eq!(missing.category().http_status(), 404);

    // Another tenant's bundle is not visible to Bob: absent, not denied.
    let hidden = h
        .service
        .assign(&bob(), AssignmentSpec::new(bundle.id, TENANT_B))
        .await
        .unwrap_err();
    assert_eq!(hidden.category().http_status(), 404);

    // Read and author without the publish capability.
    h.pdp.set_mode(PdpMode::Allow(Some(
        [actions::READ, actions::AUTHOR]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )));
    let denied = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A))
        .await
        .unwrap_err();
    assert_eq!(reason_of(&denied).as_deref(), Some("CAPABILITY_DENIED"));
    let seen = h.pdp.seen.lock();
    assert!(seen.iter().any(|r| r.action == actions::PUBLISH));
}

#[tokio::test]
async fn assignment_behind_or_beyond_a_barrier_is_refused_with_the_boundary_cause() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    for tenant in [TENANT_S, TENANT_S1, TENANT_B, ROOT, UNKNOWN_TENANT] {
        let err = h
            .service
            .assign(&a, AssignmentSpec::new(bundle.id, tenant))
            .await
            .unwrap_err();
        is_boundary(&err);
    }
    // The tenant itself and a descendant not behind a barrier are fine.
    for tenant in [TENANT_A, TENANT_A1] {
        h.service
            .assign(&a, AssignmentSpec::new(bundle.id, tenant))
            .await
            .unwrap();
    }
    // The self-managed tenant administers inside itself.
    let (own, _) = h.active(&sam(), &["own"]).await;
    h.service
        .assign(&sam(), AssignmentSpec::new(own.id, TENANT_S1))
        .await
        .unwrap();

    // A hierarchy outage is an outage, never a boundary or a grant.
    h.tree
        .fail_with(PortError::Unavailable("tenant resolver down".to_owned()));
    let down = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap_err();
    assert_eq!(down.category().http_status(), 503);
}

#[tokio::test]
async fn an_assignment_behind_a_barrier_is_not_administered() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let above = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A))
        .await
        .unwrap();
    let behind = stored_at(&h, bundle.id, TENANT_S).await;

    is_boundary(&h.service.get_assignment(&a, behind.id).await.unwrap_err());
    is_boundary(
        &h.service
            .update_assignment(&a, behind.id, false)
            .await
            .unwrap_err(),
    );
    is_boundary(&h.service.unassign(&a, behind.id).await.unwrap_err());

    // The assignment above the barrier stays readable.
    assert_eq!(h.service.get_assignment(&a, above.id).await.unwrap(), above);

    // A caller that cannot read the bundle learns nothing about it.
    assert_eq!(
        h.service
            .get_assignment(&bob(), above.id)
            .await
            .unwrap_err()
            .category()
            .http_status(),
        404
    );
}

#[tokio::test]
async fn an_assignment_outside_the_callers_reach_is_not_found_and_not_unassigned() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    // TENANT_B is a sibling subtree under ROOT: reachable neither with
    // barriers respected nor ignored. An assignment stored there must be
    // treated as absent on every surface.
    let outside = stored_at(&h, bundle.id, TENANT_B).await;

    assert_eq!(
        h.service
            .get_assignment(&a, outside.id)
            .await
            .unwrap_err()
            .category()
            .http_status(),
        404
    );
    assert_eq!(
        h.service
            .update_assignment(&a, outside.id, false)
            .await
            .unwrap_err()
            .category()
            .http_status(),
        404
    );
    // Alice administers the assignment but cannot reach its tenant:
    // unassigning is refused, never a success that leaves it enforcing.
    assert_eq!(
        h.service.unassign(&a, outside.id).await.unwrap_err(),
        assignment_not_found(outside.id)
    );
    let conn = h.db.conn().unwrap();
    assert!(
        crate::domain::repos::AssignmentRepository::get(
            &OrmAssignmentRepository,
            &conn,
            &AccessScope::allow_all(),
            outside.id
        )
        .await
        .unwrap()
        .is_some()
    );
}

#[tokio::test]
async fn changing_or_withdrawing_an_assignment_needs_publish() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let assignment = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A))
        .await
        .unwrap();

    // Read and author without the publish capability.
    h.pdp.set_mode(PdpMode::Allow(Some(
        [actions::READ, actions::AUTHOR]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )));
    let update = h
        .service
        .update_assignment(&a, assignment.id, false)
        .await
        .unwrap_err();
    assert_eq!(update, ManagementError::CapabilityDenied);
    // A denial is not an absence: unassign must not swallow it as one.
    let unassign = h.service.unassign(&a, assignment.id).await.unwrap_err();
    assert_eq!(unassign, ManagementError::CapabilityDenied);

    h.pdp.set_mode(PdpMode::Allow(None));
    let kept = h.service.get_assignment(&a, assignment.id).await.unwrap();
    assert!(kept.enforce, "the refused update changed nothing");
}

#[tokio::test]
async fn reading_an_assignment_enforces_the_scope_of_the_grant() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let created = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();

    // A granted read constrained to an unrelated resource does not cover
    // the assignment's bundle.
    h.pdp
        .set_mode(PdpMode::AllowResources(vec![Uuid::new_v4()]));
    assert_eq!(
        h.service.get_assignment(&a, created.id).await.unwrap_err(),
        assignment_not_found(created.id)
    );

    // Constrained to the bundle itself, it does.
    h.pdp.set_mode(PdpMode::AllowResources(vec![bundle.id.0]));
    assert_eq!(
        h.service.get_assignment(&a, created.id).await.unwrap(),
        created
    );
}

#[tokio::test]
async fn assignment_changes_record_their_actor() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let created = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();
    assert_eq!((created.created_by, created.updated_by), (ALICE, ALICE));

    let colleague = Uuid::from_u128(0xC011);
    let updated = h
        .service
        .update_assignment(&ctx(colleague, TENANT_A), created.id, false)
        .await
        .unwrap();
    assert_eq!((updated.created_by, updated.updated_by), (ALICE, colleague));
}

#[tokio::test]
async fn a_dormant_assignment_is_withdrawn_by_its_target_tenant() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    // Made by tenant A at S, which then became self-managed: A no longer
    // reaches S, and the assignment applies nowhere.
    let dormant = stored_at(&h, bundle.id, TENANT_S).await;

    // Its owner is past the barrier.
    is_boundary(&h.service.unassign(&a, dormant.id).await.unwrap_err());
    // S's administrator withdraws it.
    h.service.unassign(&sam(), dormant.id).await.unwrap();
    let conn = h.db.conn().unwrap();
    assert!(
        crate::domain::repos::AssignmentRepository::get(
            &OrmAssignmentRepository,
            &conn,
            &AccessScope::allow_all(),
            dormant.id
        )
        .await
        .unwrap()
        .is_none()
    );
}

#[tokio::test]
async fn a_target_tenant_cannot_withdraw_an_assignment_that_still_applies() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let applying = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();

    // A1's administrator holds publish on A1, which does not cover A's
    // assignment: refused, never a success that leaves it in place.
    let a1_admin = ctx(Uuid::from_u128(0xA1AD), TENANT_A1);
    assert_eq!(
        h.service
            .unassign(&a1_admin, applying.id)
            .await
            .unwrap_err(),
        ManagementError::CapabilityDenied
    );
    assert_eq!(
        h.service.get_assignment(&a, applying.id).await.unwrap(),
        applying
    );
}

/// Whether the assignment row exists, whoever may see it.
async fn is_stored(h: &Harness, id: AssignmentId) -> bool {
    let conn = h.db.conn().unwrap();
    crate::domain::repos::AssignmentRepository::get(
        &OrmAssignmentRepository,
        &conn,
        &AccessScope::allow_all(),
        id,
    )
    .await
    .unwrap()
    .is_some()
}

#[tokio::test]
async fn only_a_grant_covering_the_target_tenant_withdraws_a_dormant_assignment() {
    // Tenant A assigned to S1 while it reached S1; S then became self-managed,
    // so the assignment applies nowhere. Both S and S1 reach S1.
    let h = harness().await;
    let (bundle, _) = h.active(&alice(), &["deny_delete"]).await;
    let dormant = stored_at(&h, bundle.id, TENANT_S1).await;
    // Sam's grant covers S's content, not S1's: refused, the row stays.
    assert_eq!(
        h.service.unassign(&sam(), dormant.id).await.unwrap_err(),
        ManagementError::CapabilityDenied
    );
    assert!(is_stored(&h, dormant.id).await);

    // S1's administrator holds publish on S1 and withdraws it.
    let s1_admin = ctx(Uuid::from_u128(0x51AD), TENANT_S1);
    h.service.unassign(&s1_admin, dormant.id).await.unwrap();
    assert!(!is_stored(&h, dormant.id).await);
}

#[tokio::test]
async fn a_constrained_grant_withdraws_a_dormant_assignment_only_when_it_covers_the_target() {
    // A assigned to S1 while it reached S1; S then became self-managed.
    let s1_admin = ctx(Uuid::from_u128(0x51AD), TENANT_S1);

    // `static-authz` shape: every grant constrained to the caller's tenant.
    let h = harness().await;
    let (bundle, _) = h.active(&alice(), &["deny_delete"]).await;
    let dormant = stored_at(&h, bundle.id, TENANT_S1).await;
    h.pdp.set_mode(PdpMode::OwnTenantConstrained);
    assert_eq!(
        h.service.unassign(&sam(), dormant.id).await.unwrap_err(),
        ManagementError::CapabilityDenied
    );
    assert!(is_stored(&h, dormant.id).await);
    h.service.unassign(&s1_admin, dormant.id).await.unwrap();
    assert!(!is_stored(&h, dormant.id).await);

    // `tr-authz` shape: Sam administers S and S1, so the grant for S1's
    // content names S1 and covers the row.
    let h = harness().await;
    let (bundle, _) = h.active(&alice(), &["deny_delete"]).await;
    let dormant = stored_at(&h, bundle.id, TENANT_S1).await;
    h.pdp
        .set_mode(PdpMode::Administers(vec![TENANT_S, TENANT_S1]));
    h.service.unassign(&sam(), dormant.id).await.unwrap();
    assert!(!is_stored(&h, dormant.id).await);
}
