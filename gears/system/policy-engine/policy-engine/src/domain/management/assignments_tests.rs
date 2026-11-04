#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Assignment surface tests against in-memory `SQLite` with every migration
//! applied, the fake tenant tree and the fake PDP.

use std::sync::atomic::Ordering;

use policy_engine_sdk::gts::permissions::actions;
use policy_engine_sdk::management::{AssignmentSpec, ManagementError, PublishOperation};
use toolkit_db::secure::AccessScope;
use toolkit_odata::{CursorV1, ODataQuery, parse_filter_string};
use uuid::Uuid;

use super::*;
use crate::domain::management::error::reason_of;
use crate::domain::management::test_support::governance::harness_over;
use crate::domain::management::test_support::governance::{
    ALICE, Harness, ROOT, TENANT_A, TENANT_A1, TENANT_B, TENANT_S, TENANT_S1, UNKNOWN_TENANT,
    alice, bob, harness, sam,
};
use crate::domain::management::test_support::{FakePdp, PdpMode, ctx};
use crate::domain::model::BundleId;
use crate::domain::ports::PortError;
use crate::infra::storage::content_repo::OrmAssignmentRepository;
use crate::test_support::FakeTree;

fn is_boundary(err: &ManagementError) {
    assert_eq!(err.category().http_status(), 403, "{err:?}");
    assert_eq!(reason_of(err).as_deref(), Some("TENANT_BOUNDARY"));
}

/// An assignment stored directly, as one made before its tenant raised a
/// barrier would be.
async fn stored_at(h: &Harness, bundle_id: BundleId, tenant: Uuid) -> Assignment {
    h.stored_by(bundle_id, TENANT_A, tenant).await
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

    // Its owner still sees it, dormant, but can no longer change it.
    let read = h.service.get_assignment(&a, behind.id).await.unwrap();
    assert!(read.dormant, "behind a barrier it no longer applies");
    assert_eq!(read.tenant_id, TENANT_S);
    assert_eq!(read.owner_tenant_id, TENANT_A);
    is_boundary(
        &h.service
            .update_assignment(&a, behind.id, false)
            .await
            .unwrap_err(),
    );
    is_boundary(&h.service.unassign(&a, behind.id).await.unwrap_err());

    // The assignment above the barrier stays readable, and applies.
    assert_eq!(h.service.get_assignment(&a, above.id).await.unwrap(), above);
    assert!(!above.dormant);

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
    h.stored_assignment(id).await.is_some()
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

// -- Listing -------------------------------------------------------------------

/// The administrator of the root tenant.
fn root_admin() -> SecurityContext {
    ctx(Uuid::from_u128(0x2007), ROOT)
}

/// The caller's first page with no filter, as `(id, dormant)` pairs.
async fn listed(h: &Harness, caller: &SecurityContext) -> Vec<(AssignmentId, bool)> {
    h.service
        .list_assignments(caller, &ODataQuery::default())
        .await
        .unwrap()
        .items
        .iter()
        .map(|a| (a.id, a.dormant))
        .collect()
}

fn filtered(filter: &str) -> ODataQuery {
    ODataQuery::default().with_filter(parse_filter_string(filter).unwrap().into_expr())
}

#[tokio::test]
async fn the_owner_lists_its_live_and_dormant_assignments() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let live = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();
    let behind = stored_at(&h, bundle.id, TENANT_S).await;
    let below_behind = stored_at(&h, bundle.id, TENANT_S1).await;
    let outside = stored_at(&h, bundle.id, TENANT_B).await;

    let mut seen = listed(&h, &a).await;
    seen.sort_by_key(|(id, _)| id.0);
    let mut expected = vec![(live.id, false), (behind.id, true), (below_behind.id, true)];
    expected.sort_by_key(|(id, _)| id.0);
    assert_eq!(seen, expected, "a target outside the subtree is not shown");
    assert!(!seen.iter().any(|(id, _)| *id == outside.id));
}

#[tokio::test]
async fn a_target_admin_lists_the_dormant_assignments_it_may_withdraw() {
    let h = harness().await;
    let (bundle, _) = h.active(&alice(), &["deny_delete"]).await;
    let dormant = stored_at(&h, bundle.id, TENANT_S).await;

    assert_eq!(listed(&h, &sam()).await, vec![(dormant.id, true)]);
    let read = h.service.get_assignment(&sam(), dormant.id).await.unwrap();
    assert!(read.dormant);
    assert_eq!(
        read.bundle_id, bundle.id,
        "shown though the bundle is not readable"
    );
    assert_eq!(read.owner_tenant_id, TENANT_A);

    // What the listing shows, the target admin can withdraw.
    h.service.unassign(&sam(), dormant.id).await.unwrap();
    assert!(listed(&h, &sam()).await.is_empty());
}

#[tokio::test]
async fn a_target_admin_sees_only_the_dormant_rows_its_grant_covers() {
    let h = harness().await;
    let (bundle, _) = h.active(&alice(), &["deny_delete"]).await;
    let at_s = stored_at(&h, bundle.id, TENANT_S).await;
    let at_s1 = stored_at(&h, bundle.id, TENANT_S1).await;

    // A grant on S's own content alone (as `static-authz` answers).
    assert_eq!(listed(&h, &sam()).await, vec![(at_s.id, true)]);

    // A grant on S and S1 (as `tr-authz` answers for S's subtree).
    h.pdp
        .set_mode(PdpMode::Administers(vec![TENANT_S, TENANT_S1]));
    let mut seen = listed(&h, &sam()).await;
    seen.sort_by_key(|(id, _)| id.0);
    let mut expected = vec![(at_s.id, true), (at_s1.id, true)];
    expected.sort_by_key(|(id, _)| id.0);
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn an_ancestors_live_assignment_is_never_shown_as_withdrawable() {
    let h = harness().await;
    let root = root_admin();
    let (bundle, _) = h.active(&root, &["deny_delete"]).await;
    let guardrail = h
        .service
        .assign(&root, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();

    // Alice holds publish on A1, but ROOT still reaches A1: it applies,
    // and its bundle is not hers to read.
    h.pdp
        .set_mode(PdpMode::Administers(vec![TENANT_A, TENANT_A1]));
    assert!(listed(&h, &alice()).await.is_empty());
    assert_eq!(
        h.service
            .get_assignment(&alice(), guardrail.id)
            .await
            .unwrap_err(),
        assignment_not_found(guardrail.id)
    );

    h.pdp.set_mode(PdpMode::Allow(None));
    assert_eq!(listed(&h, &root).await, vec![(guardrail.id, false)]);
}

#[tokio::test]
async fn an_ancestors_assignment_of_a_readable_bundle_is_shown_live() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let by_root = h.stored_by(bundle.id, ROOT, TENANT_A1).await;
    let by_root_behind = h.stored_by(bundle.id, ROOT, TENANT_S).await;

    let mut seen = listed(&h, &a).await;
    seen.sort_by_key(|(id, _)| id.0);
    let mut expected = vec![(by_root.id, false), (by_root_behind.id, true)];
    expected.sort_by_key(|(id, _)| id.0);
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn the_listing_filters_by_bundle_and_by_target_tenant() {
    let h = harness().await;
    let a = alice();
    let (first, _) = h.active(&a, &["deny_delete"]).await;
    let (second, _) = h.active(&a, &["deny_delete"]).await;
    let first_a1 = h
        .service
        .assign(&a, AssignmentSpec::new(first.id, TENANT_A1))
        .await
        .unwrap();
    let first_a = h
        .service
        .assign(&a, AssignmentSpec::new(first.id, TENANT_A))
        .await
        .unwrap();
    let second_a1 = h
        .service
        .assign(&a, AssignmentSpec::new(second.id, TENANT_A1))
        .await
        .unwrap();

    let ids = |page: toolkit_odata::Page<sdk::Assignment>| {
        let mut ids: Vec<Uuid> = page.items.iter().map(|x| x.id.0).collect();
        ids.sort_unstable();
        ids
    };
    let mut expected = vec![first_a1.id.0, first_a.id.0];
    expected.sort_unstable();
    let by_bundle = h
        .service
        .list_assignments(&a, &filtered(&format!("bundle_id eq {}", first.id.0)))
        .await
        .unwrap();
    assert_eq!(ids(by_bundle), expected);

    let mut expected = vec![first_a1.id.0, second_a1.id.0];
    expected.sort_unstable();
    let by_tenant = h
        .service
        .list_assignments(&a, &filtered(&format!("tenant_id eq {TENANT_A1}")))
        .await
        .unwrap();
    assert_eq!(ids(by_tenant), expected);

    // Naming a tenant out of reach is an empty page, not an error.
    let elsewhere = h
        .service
        .list_assignments(&a, &filtered(&format!("tenant_id eq {TENANT_B}")))
        .await
        .unwrap();
    assert!(elsewhere.items.is_empty());
}

#[tokio::test]
async fn a_malformed_listing_filter_is_an_invalid_query() {
    let h = harness().await;
    let err = h
        .service
        .list_assignments(&alice(), &filtered("enforce eq true"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ManagementError::InvalidQuery { .. }),
        "an unknown field is refused: {err:?}"
    );
    assert_eq!(err.category().http_status(), 400);
}

#[tokio::test]
async fn assignments_are_listed_a_page_at_a_time_newest_identity_first() {
    let h = harness().await;
    let a = alice();
    let mut created = Vec::new();
    for _ in 0..3 {
        let (bundle, _) = h.active(&a, &["deny_delete"]).await;
        created.push(
            h.service
                .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
                .await
                .unwrap()
                .id,
        );
    }
    created.sort_by_key(|id| std::cmp::Reverse(id.0));

    let mut seen = Vec::new();
    let mut query = ODataQuery::default().with_limit(2);
    loop {
        let page = h.service.list_assignments(&a, &query).await.unwrap();
        assert!(page.items.len() <= 2);
        seen.extend(page.items.iter().map(|x| x.id));
        match page.page_info.next_cursor {
            Some(cursor) => {
                query = ODataQuery::default()
                    .with_limit(2)
                    .with_cursor(CursorV1::decode(&cursor).unwrap());
            }
            None => break,
        }
    }
    assert_eq!(seen, created);
}

#[tokio::test]
async fn listing_needs_read_or_publish() {
    let h = harness().await;
    let (bundle, _) = h.active(&alice(), &["deny_delete"]).await;
    let assignment = h
        .service
        .assign(&alice(), AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();

    h.pdp.set_mode(PdpMode::Deny);
    assert_eq!(
        h.service
            .list_assignments(&alice(), &ODataQuery::default())
            .await
            .unwrap_err(),
        ManagementError::CapabilityDenied
    );
    assert_eq!(
        h.service
            .get_assignment(&alice(), assignment.id)
            .await
            .unwrap_err(),
        assignment_not_found(assignment.id)
    );

    // Publish alone shows no administered row: those are read as content.
    h.pdp.set_mode(PdpMode::Allow(Some(
        [actions::PUBLISH.to_owned()].into_iter().collect(),
    )));
    assert!(listed(&h, &alice()).await.is_empty());
}

#[tokio::test]
async fn listing_fails_closed_when_a_dependency_cannot_answer() {
    let h = harness().await;
    h.pdp.set_mode(PdpMode::Fail);
    let pdp_down = h
        .service
        .list_assignments(&alice(), &ODataQuery::default())
        .await
        .unwrap_err();
    assert_eq!(pdp_down.category().http_status(), 503);

    h.pdp.set_mode(PdpMode::Allow(None));
    h.tree
        .fail_with(PortError::Unavailable("tenant resolver down".to_owned()));
    let tree_down = h
        .service
        .list_assignments(&alice(), &ODataQuery::default())
        .await
        .unwrap_err();
    assert_eq!(tree_down.category().http_status(), 503);
}

#[tokio::test]
async fn a_caller_whose_tenant_the_hierarchy_does_not_know_sees_nothing() {
    let h = harness().await;
    let (bundle, _) = h.active(&alice(), &["deny_delete"]).await;
    stored_at(&h, bundle.id, TENANT_S).await;

    let stranger = ctx(Uuid::from_u128(0x5742), UNKNOWN_TENANT);
    assert!(listed(&h, &stranger).await.is_empty());
}

#[tokio::test]
async fn reading_an_assignment_admits_exactly_what_the_listing_shows() {
    let h = harness().await;
    let a = alice();
    let root = ctx(Uuid::from_u128(0x2007), ROOT);
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let (second, _) = h.active(&a, &["deny_delete"]).await;
    let (root_bundle, _) = h.active(&root, &["deny_delete"]).await;
    let rows = [
        h.service
            .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
            .await
            .unwrap()
            .id,
        stored_at(&h, bundle.id, TENANT_S).await.id,
        stored_at(&h, bundle.id, TENANT_S1).await.id,
        stored_at(&h, bundle.id, TENANT_B).await.id,
        h.stored_by(second.id, ROOT, TENANT_A1).await.id,
        h.service
            .assign(&root, AssignmentSpec::new(root_bundle.id, TENANT_B))
            .await
            .unwrap()
            .id,
    ];
    let modes = [
        PdpMode::Allow(None),
        PdpMode::Administers(vec![TENANT_S, TENANT_S1]),
        // The shape `tr-authz` grants Alice: her tenant and its subtree.
        PdpMode::Administers(vec![TENANT_A, TENANT_A1]),
    ];
    for mode in modes {
        h.pdp.set_mode(mode.clone());
        for caller in [alice(), sam(), bob(), root.clone()] {
            let listed = listed(&h, &caller).await;
            for id in rows {
                let read = h.service.get_assignment(&caller, id).await;
                let in_list = listed.iter().find(|(listed, _)| *listed == id);
                assert_eq!(
                    read.as_ref().ok().map(|a| (a.id, a.dormant)),
                    in_list.copied(),
                    "{mode:?}, caller {}: {id:?} read {read:?}",
                    caller.subject_tenant_id()
                );
            }
        }
    }
}

#[tokio::test]
async fn a_subtree_too_large_to_list_is_refused_but_its_assignments_read() {
    // A root with more descendants than one statement can bind.
    let children: Vec<Uuid> = (0..16_000).map(|_| Uuid::new_v4()).collect();
    let mut nodes = vec![(ROOT, None, false)];
    nodes.extend(children.iter().map(|&child| (child, Some(ROOT), false)));
    let h = harness_over(FakePdp::allow_all(), FakeTree::new(&nodes)).await;
    let root = ctx(Uuid::from_u128(0x2007), ROOT);
    let (bundle, _) = h.active(&root, &["deny_delete"]).await;
    let assigned = h
        .service
        .assign(&root, AssignmentSpec::new(bundle.id, children[0]))
        .await
        .unwrap();

    let err = h
        .service
        .list_assignments(&root, &ODataQuery::default())
        .await
        .unwrap_err();
    assert_eq!(err, ManagementError::SubtreeTooLarge);
    assert_eq!(err.category().http_status(), 400);
    assert_eq!(reason_of(&err).as_deref(), Some("SUBTREE_TOO_LARGE"));

    let read = h.service.get_assignment(&root, assigned.id).await.unwrap();
    assert_eq!(read, assigned);
}

#[tokio::test]
async fn reading_an_assignment_never_reads_the_callers_subtree() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let live = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();
    let dormant = stored_at(&h, bundle.id, TENANT_S).await;

    h.service.get_assignment(&a, live.id).await.unwrap();
    h.service.get_assignment(&sam(), dormant.id).await.unwrap();
    h.service.get_assignment(&bob(), live.id).await.unwrap_err();
    assert_eq!(
        h.tree.subtree_calls.load(Ordering::SeqCst),
        0,
        "one row is decided by point checks"
    );
}

#[tokio::test]
async fn a_visible_assignment_the_caller_may_not_change_is_refused() {
    // Alice administers A and its subtree; ROOT assigned her bundle at A1.
    // She sees it (its owner is in her chain), but publish on ROOT's
    // content is not hers: every change is refused, never reported absent
    // or done.
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let by_root = h.stored_by(bundle.id, ROOT, TENANT_A1).await;
    h.pdp
        .set_mode(PdpMode::Administers(vec![TENANT_A, TENANT_A1]));
    assert_eq!(listed(&h, &a).await, vec![(by_root.id, false)]);
    assert!(
        !h.service
            .get_assignment(&a, by_root.id)
            .await
            .unwrap()
            .dormant
    );

    let unassign = PublishOperation::Unassign {
        assignment_id: by_root.id,
    };
    assert_eq!(
        h.service.check_publish(&a, unassign).await.unwrap_err(),
        ManagementError::CapabilityDenied
    );
    assert_eq!(
        h.service.unassign(&a, by_root.id).await.unwrap_err(),
        ManagementError::CapabilityDenied
    );
    assert!(
        h.stored_assignment(by_root.id).await.is_some(),
        "still stored"
    );
    assert_eq!(
        h.service
            .update_assignment(&a, by_root.id, false)
            .await
            .unwrap_err(),
        ManagementError::CapabilityDenied
    );
    assert!(h.stored_assignment(by_root.id).await.unwrap().enforce);

    // Bob sees none of it: absent to him, as before.
    assert!(matches!(
        h.service
            .update_assignment(&bob(), by_root.id, false)
            .await
            .unwrap_err(),
        ManagementError::NotFound { .. }
    ));
    h.service.unassign(&bob(), by_root.id).await.unwrap();
    assert!(h.stored_assignment(by_root.id).await.is_some());
}

#[tokio::test]
async fn an_update_the_hierarchy_cannot_complete_changes_nothing() {
    let h = harness().await;
    let a = alice();
    let (bundle, _) = h.active(&a, &["deny_delete"]).await;
    let first = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A1))
        .await
        .unwrap();
    let second = h
        .service
        .assign(&a, AssignmentSpec::new(bundle.id, TENANT_A))
        .await
        .unwrap();
    // How many hierarchy calls one update makes.
    let before = h.tree.calls.load(Ordering::SeqCst);
    h.service
        .update_assignment(&a, first.id, false)
        .await
        .unwrap();
    let per_update = h.tree.calls.load(Ordering::SeqCst) - before;

    // The hierarchy fails on the last of them: the refusal comes before the
    // write, never after it.
    let start = h.tree.calls.load(Ordering::SeqCst);
    h.tree.fail_from(
        start + per_update - 1,
        PortError::Unavailable("tenant resolver down".to_owned()),
    );
    let err = h
        .service
        .update_assignment(&a, second.id, false)
        .await
        .unwrap_err();
    assert_eq!(err.category().http_status(), 503, "{err:?}");
    let stored = h.stored_assignment(second.id).await.unwrap();
    assert!(stored.enforce, "nothing was written");
    assert_eq!(stored.updated_at, second.updated_at);
}
