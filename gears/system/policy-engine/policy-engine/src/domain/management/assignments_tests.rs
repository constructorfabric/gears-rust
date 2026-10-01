#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Assignment surface tests against in-memory `SQLite` with every migration
//! applied, the fake tenant tree and the fake PDP.

use policy_engine_sdk::gts::permissions::actions;
use policy_engine_sdk::management::{AssignmentSpec, ManagementError, reason};
use time::OffsetDateTime;
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

use super::*;
use crate::domain::management::error::reason_of;
use crate::domain::management::test_support::PdpMode;
use crate::domain::management::test_support::governance::{
    Harness, ROOT, TENANT_A, TENANT_A1, TENANT_B, TENANT_S, TENANT_S1, UNKNOWN_TENANT, alice, bob,
    harness, sam,
};
use crate::infra::storage::content_repo::OrmAssignmentRepository;

fn code(err: &ManagementError) -> Option<String> {
    reason_of(err)
}

fn is_boundary(err: &ManagementError) {
    assert_eq!(err.status_code(), 403, "{err:?}");
    assert_eq!(code(err).as_deref(), Some(reason::TENANT_BOUNDARY));
}

/// An assignment stored directly, as one made before its tenant raised a
/// barrier would be.
async fn stored_at(h: &Harness, bundle_id: Uuid, tenant: Uuid) -> Assignment {
    let conn = h.db.conn().unwrap();
    let at = OffsetDateTime::now_utc();
    OrmAssignmentRepository
        .insert(
            &conn,
            &AccessScope::allow_all(),
            &Assignment {
                id: AssignmentId(Uuid::new_v4()),
                bundle_id: BundleId(bundle_id),
                tenant_id: tenant,
                owner_tenant_id: TENANT_A,
                enforce: true,
                created_at: at,
                updated_at: at,
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
    assert_eq!(exists.status_code(), 409);
    assert_eq!(code(&exists).as_deref(), Some(reason::ASSIGNMENT_EXISTS));

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
            .status_code(),
        404
    );
    h.service.unassign(&a, created.id).await.unwrap();
    h.service.unassign(&a, Uuid::new_v4()).await.unwrap();
}

#[tokio::test]
async fn assign_checks_bundle_and_capability() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let missing = h
        .service
        .assign(&a, AssignmentSpec::new(Uuid::new_v4(), TENANT_A))
        .await
        .unwrap_err();
    assert_eq!(missing.status_code(), 404);

    // Another tenant's bundle is not visible to Bob: absent, not denied.
    let hidden = h
        .service
        .assign(&bob(), AssignmentSpec::new(bundle.id, TENANT_B))
        .await
        .unwrap_err();
    assert_eq!(hidden.status_code(), 404);

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
    assert_eq!(code(&denied).as_deref(), Some(reason::CAPABILITY_DENIED));
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
    h.tree.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let down = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap_err();
    assert_eq!(down.status_code(), 503);
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

    is_boundary(&h.service.get_assignment(&a, behind.id.0).await.unwrap_err());
    is_boundary(
        &h.service
            .update_assignment(&a, behind.id.0, false)
            .await
            .unwrap_err(),
    );
    is_boundary(&h.service.unassign(&a, behind.id.0).await.unwrap_err());

    // The assignment above the barrier stays readable.
    assert_eq!(h.service.get_assignment(&a, above.id).await.unwrap(), above);

    // A caller that cannot read the bundle learns nothing about it.
    assert_eq!(
        h.service
            .get_assignment(&bob(), above.id)
            .await
            .unwrap_err()
            .status_code(),
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
            .get_assignment(&a, outside.id.0)
            .await
            .unwrap_err()
            .status_code(),
        404
    );
    assert_eq!(
        h.service
            .update_assignment(&a, outside.id.0, false)
            .await
            .unwrap_err()
            .status_code(),
        404
    );
    // Unassigning looks like unassigning a missing assignment: success, and
    // the assignment stays.
    h.service.unassign(&a, outside.id.0).await.unwrap();
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
