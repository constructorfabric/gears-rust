//! Real-DB integration tests for the conversion-request lifecycle
//! exercised end-to-end against in-memory `SQLite` with the production
//! migration set + SeaORM-backed `ConversionRepoImpl`.
//!
//! Scope:
//!
//! 1. Happy-path approve roundtrip — `request_conversion` then
//!    `approve` flips `tenants.self_managed` and rewrites the
//!    affected `tenant_closure.barrier` rows in one TX.
//! 2. Single-pending unique-index — second insert against the same
//!    tenant rejects on the partial-unique index
//!    `ux_conversion_requests_pending`.
//! 3. Dual-consent matrix — the four resolutions (approve / cancel /
//!    reject / expire) against the right-side and wrong-side actor;
//!    wrong-side calls return `InvalidActorForTransition`, right-side
//!    transitions land.
//! 4. Expire tick — a pending row with `expires_at` in the past
//!    transitions to `Expired` after one `expire_pending` call.
//! 5. Retention — `soft_delete_resolved` only stamps `deleted_at` on
//!    rows whose `resolved_at` precedes the configured cutoff.
//! 6. Listings — `list_own_for_tenant` and `list_inbound_for_parent`
//!    return correct shapes against a multi-tenant fixture.
//!
//! Out of scope on `SQLite` (covered on Postgres in
//! `conversion_integration_pg.rs::pg_concurrent_approves_serialize_to_one_winner`):
//! the dual-consent apply's final-stamp `rows_affected == 0` fence on
//! the conversion-request transition. That fence guards against a peer
//! apply landing first under concurrent counterparty approves; the only
//! way to drive it is real `SERIALIZABLE` contention, which `SQLite`
//! does not provide. The fence itself is unit-tested at the repo seam
//! (the fake's `apply_conversion_approval` returns `AlreadyResolved` on
//! status drift), and the production SQL impl wires the same fence.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::Arc;
use std::time::Duration as StdDuration;

use account_management::domain::conversion::model::{
    ConversionPagination, ConversionSide, ConversionStatus, NewConversionRequest, TargetMode,
};
use account_management::domain::conversion::repo::ConversionRepo;
use account_management::domain::conversion::service::{
    ConversionCaller, ConversionScope, ConversionService, ListConversionsQuery,
    RequestConversionInput,
};
use account_management::domain::error::DomainError;
use account_management::domain::tenant::TenantRepo;
use account_management::domain::tenant::closure::build_activation_rows;
use account_management::domain::tenant::model::{NewTenant, TenantStatus};
use account_management::domain::tenant_type::inert_tenant_type_checker;
use account_management::infra::storage::repo_impl::{AmDbProvider, ConversionRepoImpl};
use modkit_db::secure::SecureEntityExt;
use modkit_security::AccessScope;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use time::{Duration as TimeDuration, OffsetDateTime};
use uuid::Uuid;

use common::*;

const APPROVAL_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

fn approval_ttl() -> StdDuration {
    StdDuration::from_secs(APPROVAL_TTL_SECS)
}

fn retention_window() -> StdDuration {
    StdDuration::from_secs(RETENTION_SECS)
}

/// Drive the full create-child saga (steps 1 + 3) so `tenant_id`
/// lands in `Active` with the closure rows the activation contract
/// requires.
async fn create_active_child(
    h: &Harness,
    tenant_id: Uuid,
    parent_id: Uuid,
    name: &str,
    self_managed: bool,
    depth: u32,
) {
    let new = NewTenant {
        id: tenant_id,
        parent_id: Some(parent_id),
        name: name.to_owned(),
        self_managed,
        tenant_type_uuid: Uuid::from_u128(0xAA),
        depth,
    };
    h.repo
        .insert_provisioning(&allow_all(), &new)
        .await
        .expect("insert_provisioning");
    let ancestor_chain = h
        .repo
        .load_ancestor_chain_through_parent(&allow_all(), parent_id)
        .await
        .expect("ancestor chain");
    let closure_rows = build_activation_rows(
        tenant_id,
        TenantStatus::Active,
        self_managed,
        &ancestor_chain,
    );
    h.repo
        .activate_tenant(&allow_all(), tenant_id, &closure_rows, None)
        .await
        .expect("activate_tenant");
}

/// Seed the platform root tenant + its self-row directly without
/// driving the bootstrap saga.
async fn seed_root(h: &Harness, root_id: Uuid) {
    insert_tenant(&h.provider, root_id, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root");
    insert_closure(&h.provider, root_id, root_id, 0, ACTIVE)
        .await
        .expect("seed root self-row");
}

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch")
}

/// Build a wired conversion service with the production
/// `ConversionRepoImpl` + an inert tenant-type checker.
fn build_service(
    h: &Harness,
    now: OffsetDateTime,
) -> (Arc<ConversionService>, Arc<ConversionRepoImpl>) {
    let conv_repo = Arc::new(ConversionRepoImpl::new(Arc::clone(&h.provider)));
    let conv_repo_dyn: Arc<dyn ConversionRepo> = conv_repo.clone();
    let tenant_repo: Arc<dyn TenantRepo> = h.repo.clone();
    let now_fn = Arc::new(move || now);
    let svc = Arc::new(
        ConversionService::new(
            conv_repo_dyn,
            tenant_repo,
            inert_tenant_type_checker(),
            mock_enforcer(),
            approval_ttl(),
            retention_window(),
        )
        .with_now_fn(now_fn),
    );
    (svc, conv_repo)
}

// ---------------------------------------------------------------------
// 1. Happy path — approve flips self_managed AND rewrites closure barrier.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_approve_flips_self_managed_and_barrier() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await; // managed

    let (svc, _conv_repo) = build_service(&h, fixed_now());

    let initiated = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect("request_conversion");
    assert_eq!(initiated.target_mode, TargetMode::SelfManaged);

    let approved = svc
        .approve(&ctx_for(root), initiated.id, ConversionCaller::parent(root))
        .await
        .expect("approve");
    assert_eq!(approved.status, ConversionStatus::Approved);
    assert!(approved.approved_by.is_some());

    // tenants.self_managed flipped.
    let row = fetch_tenant(&h.provider, child)
        .await
        .unwrap()
        .expect("tenant row");
    assert!(
        row.self_managed,
        "tenants.self_managed flipped to true after approve"
    );

    // (root, child) closure barrier = 1 (child is now self-managed,
    // strict path = {child}).
    let barrier_root_child = fetch_closure_row(&h.provider, root, child)
        .await
        .unwrap()
        .expect("(root, child) row")
        .barrier;
    assert_eq!(barrier_root_child, 1);
    // Self-rows always 0.
    let child_self = fetch_closure_row(&h.provider, child, child)
        .await
        .unwrap()
        .expect("child self-row")
        .barrier;
    assert_eq!(child_self, 0);
}

// ---------------------------------------------------------------------
// 2. Single-pending unique-index enforcement.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_pending_unique_index_rejects_second_insert() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await;

    let (_svc, conv_repo) = build_service(&h, fixed_now());

    let now = fixed_now();
    let new = NewConversionRequest {
        id: Uuid::new_v4(),
        tenant_id: child,
        parent_id: Some(root),
        child_tenant_name: "c".into(),
        initiator_side: ConversionSide::Child,
        target_mode: TargetMode::SelfManaged,
        requested_by: Uuid::new_v4(),
        requested_at: now,
        expires_at: now + TimeDuration::days(7),
    };
    let inserted = conv_repo
        .insert_pending(&allow_all(), &new)
        .await
        .expect("first insert");

    let dup = NewConversionRequest {
        id: Uuid::new_v4(),
        tenant_id: child,
        parent_id: Some(root),
        child_tenant_name: "c".into(),
        initiator_side: ConversionSide::Parent,
        target_mode: TargetMode::SelfManaged,
        requested_by: Uuid::new_v4(),
        requested_at: now,
        expires_at: now + TimeDuration::days(7),
    };
    let err = conv_repo
        .insert_pending(&allow_all(), &dup)
        .await
        .expect_err("second insert MUST collide");
    match err {
        DomainError::PendingExists { request_id } => {
            assert_eq!(request_id, inserted.id.to_string());
        }
        other => panic!("expected PendingExists, got {other:?}"),
    }
}

/// Pin the partial-unique semantics of `ux_conversion_requests_pending`
/// (`WHERE status = 0 AND deleted_at IS NULL`): once a prior request
/// reaches a resolved status it falls out of the index by the
/// `status != 0` predicate, freeing the per-tenant slot for a fresh
/// pending insert. The subsequent `deleted_at` stamp is independently
/// honored by the same predicate (`deleted_at IS NULL`), so even a
/// hypothetical "soft-deleted pending" row would be excluded — that
/// second leg is what this fixture documents end-to-end against the
/// `SQLite` migration branch. Production exercises this path naturally
/// on `request_conversion` after retention soft-delete; pin it here so
/// a regression in either leg of the partial predicate is caught by
/// CI.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_partial_unique_admits_reinsert_after_soft_delete() {
    use account_management::infra::storage::entity::conversion_requests;
    use modkit_db::secure::SecureUpdateExt;
    use sea_orm::sea_query::Expr;

    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await;

    let (svc, conv_repo) = build_service(&h, fixed_now());
    let now = fixed_now();

    // 1. Insert a pending request, drive it to a resolved (cancelled)
    //    state — the partial-unique index now omits this row by status.
    let first = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect("first request");
    let _ = svc
        .cancel(&ctx_for(root), first.id, ConversionCaller::child(child))
        .await
        .expect("first cancel");

    // 2. Soft-delete the prior row: stamp `deleted_at = now` directly
    //    so the COALESCE branch of the SQLite index expression is
    //    actually exercised. The retention sweep would normally do
    //    this, but a fresh row whose `resolved_at` is recent is not
    //    eligible — we drive the soft-delete by hand.
    let conn = h.provider.conn().expect("conn");
    conversion_requests::Entity::update_many()
        .col_expr(
            conversion_requests::Column::DeletedAt,
            Expr::value(Some(now)),
        )
        .filter(Condition::all().add(conversion_requests::Column::Id.eq(first.id)))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("manual soft-delete");

    // 3. A fresh `pending` insert against the same tenant MUST land —
    //    the prior soft-deleted row is excluded by the partial-unique
    //    predicate (`status = 0 AND deleted_at IS NULL`) and the
    //    COALESCE folds the `deleted_at` value into the index key so
    //    the SQLite NULL-distinct rule cannot create a duplicate.
    let second_id = Uuid::new_v4();
    conv_repo
        .insert_pending(
            &allow_all(),
            &NewConversionRequest {
                id: second_id,
                tenant_id: child,
                parent_id: Some(root),
                child_tenant_name: "c".into(),
                initiator_side: ConversionSide::Child,
                target_mode: TargetMode::SelfManaged,
                requested_by: Uuid::new_v4(),
                requested_at: now,
                expires_at: now + TimeDuration::days(7),
            },
        )
        .await
        .expect("re-insert after soft-delete must land");
}

// ---------------------------------------------------------------------
// 3. Dual-consent matrix — right-side vs. wrong-side actor.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dual_consent_matrix_actor_rules() {
    // For each of approve / cancel / reject, verify the right-side
    // call lands and the wrong-side call returns
    // `InvalidActorForTransition`. Expire is system-driven, no
    // actor matrix.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;

    let (svc, _conv_repo) = build_service(&h, fixed_now());

    // Cancel: initiator-only.
    let c1 = Uuid::new_v4();
    create_active_child(&h, c1, root, "c1", false, 1).await;
    let req_c1 = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: c1,
                caller: ConversionCaller::child(c1),
                target_mode_override: None,
            },
        )
        .await
        .expect("request c1");
    let err = svc
        .cancel(&ctx_for(root), req_c1.id, ConversionCaller::parent(root))
        .await
        .expect_err("wrong-side cancel");
    assert!(matches!(err, DomainError::InvalidActorForTransition { .. }));
    let cancelled = svc
        .cancel(&ctx_for(root), req_c1.id, ConversionCaller::child(c1))
        .await
        .expect("right-side cancel");
    assert_eq!(cancelled.status, ConversionStatus::Cancelled);

    // Reject: counterparty-only.
    let c2 = Uuid::new_v4();
    create_active_child(&h, c2, root, "c2", false, 1).await;
    let req_c2 = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: c2,
                caller: ConversionCaller::child(c2),
                target_mode_override: None,
            },
        )
        .await
        .expect("request c2");
    let err = svc
        .reject(&ctx_for(root), req_c2.id, ConversionCaller::child(c2))
        .await
        .expect_err("wrong-side reject");
    assert!(matches!(err, DomainError::InvalidActorForTransition { .. }));
    let rejected = svc
        .reject(&ctx_for(root), req_c2.id, ConversionCaller::parent(root))
        .await
        .expect("right-side reject");
    assert_eq!(rejected.status, ConversionStatus::Rejected);

    // Approve: counterparty-only.
    let c3 = Uuid::new_v4();
    create_active_child(&h, c3, root, "c3", false, 1).await;
    let req_c3 = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: c3,
                caller: ConversionCaller::child(c3),
                target_mode_override: None,
            },
        )
        .await
        .expect("request c3");
    let err = svc
        .approve(&ctx_for(root), req_c3.id, ConversionCaller::child(c3))
        .await
        .expect_err("wrong-side approve");
    assert!(matches!(err, DomainError::InvalidActorForTransition { .. }));
    let approved = svc
        .approve(&ctx_for(root), req_c3.id, ConversionCaller::parent(root))
        .await
        .expect("right-side approve");
    assert_eq!(approved.status, ConversionStatus::Approved);
}

// ---------------------------------------------------------------------
// 4. Expire tick — past `expires_at` transitions to Expired.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expire_tick_transitions_past_pending() {
    use account_management::infra::storage::entity::conversion_requests;
    use modkit_db::secure::SecureUpdateExt;
    use sea_orm::sea_query::Expr;

    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await;

    let (svc, _conv_repo) = build_service(&h, fixed_now());

    let req = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: child,
                caller: ConversionCaller::child(child),
                target_mode_override: None,
            },
        )
        .await
        .expect("request_conversion");

    // Reach behind the service to pull `expires_at` into the past
    // (production happens via wall-clock; the test pins a fixed `now`
    // and would otherwise have to wait the full TTL).
    let conn = h.provider.conn().expect("conn");
    let past = fixed_now() - TimeDuration::hours(1);
    conversion_requests::Entity::update_many()
        .col_expr(conversion_requests::Column::ExpiresAt, Expr::value(past))
        .filter(Condition::all().add(conversion_requests::Column::Id.eq(req.id)))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate expires_at");

    let count = svc
        .expire_pending(
            &ConversionScope::system_sweep(),
            100,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("expire tick");
    assert_eq!(count, 1, "exactly one row transitions");

    // Re-fetch via the service repo path.
    let conv_repo = Arc::new(ConversionRepoImpl::new(Arc::clone(&h.provider)));
    let row = conv_repo
        .find_by_id(&allow_all(), req.id)
        .await
        .expect("find_by_id")
        .expect("row exists");
    assert_eq!(row.status, ConversionStatus::Expired);
    assert!(row.resolved_at.is_some());
}

// ---------------------------------------------------------------------
// 5. Retention — `soft_delete_resolved` stamps deleted_at on old
// resolved rows only.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_soft_delete_resolved_only_old_rows() {
    use account_management::infra::storage::entity::conversion_requests;
    use modkit_db::secure::SecureUpdateExt;
    use sea_orm::sea_query::Expr;

    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let c1 = Uuid::new_v4();
    let c2 = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, c1, root, "c1", false, 1).await;
    create_active_child(&h, c2, root, "c2", false, 1).await;

    let (svc, conv_repo) = build_service(&h, fixed_now());

    // c1: cancel and backdate `resolved_at` past the cutoff (eligible).
    let req_c1 = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: c1,
                caller: ConversionCaller::child(c1),
                target_mode_override: None,
            },
        )
        .await
        .expect("request c1");
    let cancelled = svc
        .cancel(&ctx_for(root), req_c1.id, ConversionCaller::child(c1))
        .await
        .expect("cancel c1");
    assert_eq!(cancelled.status, ConversionStatus::Cancelled);

    let conn = h.provider.conn().expect("conn");
    let old = fixed_now() - TimeDuration::days(30);
    conversion_requests::Entity::update_many()
        .col_expr(
            conversion_requests::Column::ResolvedAt,
            Expr::value(Some(old)),
        )
        .filter(Condition::all().add(conversion_requests::Column::Id.eq(req_c1.id)))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate c1 resolved_at");

    // c2: cancel and KEEP `resolved_at` recent (NOT eligible).
    let req_c2 = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: c2,
                caller: ConversionCaller::child(c2),
                target_mode_override: None,
            },
        )
        .await
        .expect("request c2");
    let _ = svc
        .cancel(&ctx_for(root), req_c2.id, ConversionCaller::child(c2))
        .await
        .expect("cancel c2");

    let count = svc
        .soft_delete_resolved(&ConversionScope::system_sweep(), retention_window(), 100)
        .await
        .expect("retention sweep");
    assert_eq!(count, 1, "only c1 (old) is reaped");

    // c1 row stamped deleted_at, c2 untouched.
    let _ = conv_repo;
    let conn2 = h.provider.conn().expect("conn");
    let c1_row = conversion_requests::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(conversion_requests::Column::Id.eq(req_c1.id)))
        .one(&conn2)
        .await
        .expect("find c1")
        .expect("row");
    assert!(c1_row.deleted_at.is_some(), "c1 must be soft-deleted");
    let c2_row = conversion_requests::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(conversion_requests::Column::Id.eq(req_c2.id)))
        .one(&conn2)
        .await
        .expect("find c2")
        .expect("row");
    assert!(c2_row.deleted_at.is_none(), "c2 must remain alive");
}

// ---------------------------------------------------------------------
// 6. Listings — list_own_for_tenant + list_inbound_for_parent shapes.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listings_own_and_inbound_return_correct_shape() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let c1 = Uuid::new_v4();
    let c2 = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, c1, root, "c1", false, 1).await;
    create_active_child(&h, c2, root, "c2", false, 1).await;

    let (svc, _conv_repo) = build_service(&h, fixed_now());

    let req_c1 = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: c1,
                caller: ConversionCaller::child(c1),
                target_mode_override: None,
            },
        )
        .await
        .expect("request c1");
    let req_c2 = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: c2,
                caller: ConversionCaller::parent(root),
                target_mode_override: None,
            },
        )
        .await
        .expect("request c2");

    // Own listing for c1 only sees its own request.
    let own_c1 = svc
        .list_own_for_tenant(
            &ctx_for(root),
            c1,
            &ListConversionsQuery::any(50, 0).expect("top > 0"),
        )
        .await
        .expect("list own c1");
    assert_eq!(own_c1.items.len(), 1);
    assert_eq!(own_c1.items[0].id, req_c1.id);

    // Inbound for root sees both children's requests.
    let inbound = svc
        .list_inbound_for_parent(
            &ctx_for(root),
            root,
            &ListConversionsQuery::any(50, 0).expect("top > 0"),
        )
        .await
        .expect("list inbound root");
    let ids: Vec<Uuid> = inbound.items.iter().map(|p| p.request_id).collect();
    assert!(ids.contains(&req_c1.id));
    assert!(ids.contains(&req_c2.id));
    assert_eq!(inbound.items.len(), 2);
}

// ---------------------------------------------------------------------
// Pagination smoke (shape) — pulls the repo-level
// `list_own_for_tenant` directly to spot-check the secure filter
// pipeline interacts with `top` / `skip` against a real backend.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_level_pagination_smoke() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await;

    let conv_repo = Arc::new(ConversionRepoImpl::new(Arc::clone(&h.provider)));
    // Insert one pending row + one resolved row directly via the repo
    // so we can exercise pagination beyond the at-most-one-pending
    // invariant.
    let now = fixed_now();
    let pending_id = Uuid::new_v4();
    conv_repo
        .insert_pending(
            &allow_all(),
            &NewConversionRequest {
                id: pending_id,
                tenant_id: child,
                parent_id: Some(root),
                child_tenant_name: "c".into(),
                initiator_side: ConversionSide::Child,
                target_mode: TargetMode::SelfManaged,
                requested_by: Uuid::new_v4(),
                requested_at: now,
                expires_at: now + TimeDuration::days(7),
            },
        )
        .await
        .expect("insert pending");
    // Drive it to cancelled to free up the pending slot.
    conv_repo
        .transition_pending_to_cancelled(&allow_all(), pending_id, Uuid::new_v4(), now)
        .await
        .expect("cancel");

    // Insert another pending row.
    conv_repo
        .insert_pending(
            &allow_all(),
            &NewConversionRequest {
                id: Uuid::new_v4(),
                tenant_id: child,
                parent_id: Some(root),
                child_tenant_name: "c".into(),
                initiator_side: ConversionSide::Child,
                target_mode: TargetMode::SelfManaged,
                requested_by: Uuid::new_v4(),
                requested_at: now,
                expires_at: now + TimeDuration::days(7),
            },
        )
        .await
        .expect("insert pending #2");

    let page = conv_repo
        .list_own_for_tenant(
            &allow_all(),
            child,
            None,
            ConversionPagination { top: 50, skip: 0 },
        )
        .await
        .expect("list");
    assert_eq!(page.len(), 2, "two rows total");
    let page_top1 = conv_repo
        .list_own_for_tenant(
            &allow_all(),
            child,
            None,
            ConversionPagination { top: 1, skip: 0 },
        )
        .await
        .expect("list top=1");
    assert_eq!(page_top1.len(), 1);
}

// ---------------------------------------------------------------------
// 7. Soft-delete race / barrier recompute on retained closure rows.
// ---------------------------------------------------------------------
//
// Pin the codex-R7 P2 fixes:
//   * P2.1: `apply_conversion_approval`'s step-2 reload no longer
//     filters `deleted_at IS NULL`, so a tenant soft-deleted between
//     the service-level Active precheck and the apply TX surfaces
//     `Validation` (status != Active) instead of a misleading
//     `NotFound`.
//   * P2.2: the step-6 barrier-recompute snapshot includes soft-
//     deleted tenants too, so closure rows referencing them get
//     their barrier values updated alongside the live tree.

/// Stamp `status = Deleted` and `deleted_at = now` on a tenant row
/// directly, bypassing the production soft-delete saga (no FK
/// touched). Used to seed the soft-delete race fixture without the
/// extra moving parts of `TenantService::delete_tenant`.
async fn stamp_soft_deleted(provider: &Arc<AmDbProvider>, tenant_id: Uuid, now: OffsetDateTime) {
    use account_management::infra::storage::entity::tenants;
    use modkit_db::secure::SecureUpdateExt;
    use sea_orm::sea_query::Expr;
    let conn = provider.conn().expect("conn");
    tenants::Entity::update_many()
        .col_expr(
            tenants::Column::Status,
            Expr::value(TenantStatus::Deleted.as_smallint()),
        )
        .col_expr(tenants::Column::DeletedAt, Expr::value(Some(now)))
        .filter(Condition::all().add(tenants::Column::Id.eq(tenant_id)))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("stamp soft-delete");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_conversion_approval_on_soft_deleted_tenant_returns_validation_not_not_found() {
    use account_management::domain::conversion::repo::ApplyConversionApprovalInput;
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await;

    let conv_repo = Arc::new(ConversionRepoImpl::new(Arc::clone(&h.provider)));
    let now = fixed_now();

    // Insert a pending conversion request directly via the repo.
    let req_id = Uuid::new_v4();
    let new = NewConversionRequest {
        id: req_id,
        tenant_id: child,
        parent_id: Some(root),
        child_tenant_name: "c".into(),
        initiator_side: ConversionSide::Child,
        target_mode: TargetMode::SelfManaged,
        requested_by: Uuid::new_v4(),
        requested_at: now,
        expires_at: now + TimeDuration::days(7),
    };
    conv_repo
        .insert_pending(&allow_all(), &new)
        .await
        .expect("insert pending");

    // Simulate the soft-delete race: tenant becomes `Deleted` AFTER
    // the (skipped here) service precheck but BEFORE
    // `apply_conversion_approval` opens its TX. Calling the repo
    // method directly bypasses the service-level precheck so the
    // step-2 reload is the gate under test.
    stamp_soft_deleted(&h.provider, child, now).await;

    let err = conv_repo
        .apply_conversion_approval(
            &allow_all(),
            ApplyConversionApprovalInput {
                request_id: req_id,
                target_tenant_id: child,
                target_mode: TargetMode::SelfManaged,
                // Types match what `seed_root` / `create_active_child`
                // stamped: root=nil, child=0xAA. This test exercises
                // the soft-delete (status) branch, not the TOCTOU
                // type guard, so the values are passed-through.
                expected_tenant_type_uuid: Uuid::from_u128(0xAA),
                expected_parent_tenant_type_uuid: Uuid::nil(),
                approver_uuid: Uuid::new_v4(),
                resolved_at: now,
            },
        )
        .await
        .expect_err("soft-deleted tenant must surface Validation, not NotFound");

    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("not active"),
                "expected `not active` diagnostic, got {detail}"
            );
        }
        other => panic!(
            "expected Validation for soft-deleted tenant; the bug is surfacing NotFound -- got \
             {other:?}"
        ),
    }
}

/// Pins the partial-TX rollback for the
/// `apply_conversion_approval` TOCTOU type guard. After
/// `ConversionService::approve` ran its pre-apply type compatibility
/// check the converting tenant's `tenant_type_uuid` is mutated out
/// of band — the apply TX MUST reject with `Validation` (mismatched
/// `expected_tenant_type_uuid`) and leave every other piece of
/// state unchanged: the pending row stays `Pending`,
/// `tenants.self_managed` stays at its pre-apply value, and every
/// closure-row barrier stays put. Closes deep-review #15 partial-
/// TX rollback coverage now that type re-eval lives at the service
/// layer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_conversion_approval_rolls_back_on_tenant_type_drift_toctou() {
    use account_management::domain::conversion::repo::ApplyConversionApprovalInput;
    use account_management::infra::storage::entity::tenants;
    use modkit_db::secure::SecureUpdateExt;
    use sea_orm::sea_query::Expr;

    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await; // managed

    let conv_repo = Arc::new(ConversionRepoImpl::new(Arc::clone(&h.provider)));
    let now = fixed_now();

    // Insert pending. Both endpoints' tenant_type_uuid are the
    // values stamped by `seed_root` / `create_active_child`:
    // root = Uuid::nil(), child = Uuid::from_u128(0xAA).
    let req_id = Uuid::new_v4();
    let new = NewConversionRequest {
        id: req_id,
        tenant_id: child,
        parent_id: Some(root),
        child_tenant_name: "c".into(),
        initiator_side: ConversionSide::Child,
        target_mode: TargetMode::SelfManaged,
        requested_by: Uuid::new_v4(),
        requested_at: now,
        expires_at: now + TimeDuration::days(7),
    };
    conv_repo
        .insert_pending(&allow_all(), &new)
        .await
        .expect("insert pending");

    // Snapshot the (root, child) closure barrier + child's
    // self_managed bool BEFORE the failing apply. They MUST be
    // identical AFTER the failing apply, proving the TX rolled
    // back atomically.
    let pre_self_managed = fetch_tenant(&h.provider, child)
        .await
        .expect("fetch child pre")
        .expect("child row")
        .self_managed;
    let pre_barrier = fetch_closure_row(&h.provider, root, child)
        .await
        .expect("(root,child) closure read")
        .expect("(root,child) closure row")
        .barrier;

    // Out-of-band retype on the converting tenant — simulates a
    // peer write committing between the service's pre-apply
    // `TenantTypeChecker::check_parent_child` and the apply TX.
    let drifted_type = Uuid::from_u128(0xBB);
    let conn = h.provider.conn().expect("conn");
    tenants::Entity::update_many()
        .col_expr(tenants::Column::TenantTypeUuid, Expr::value(drifted_type))
        .filter(Condition::all().add(tenants::Column::Id.eq(child)))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("retype child out of band");

    // The service would have observed the pre-drift values; pass
    // them in unchanged so the apply TX's TOCTOU guard fires.
    let err = conv_repo
        .apply_conversion_approval(
            &allow_all(),
            ApplyConversionApprovalInput {
                request_id: req_id,
                target_tenant_id: child,
                target_mode: TargetMode::SelfManaged,
                expected_tenant_type_uuid: Uuid::from_u128(0xAA), // stale
                expected_parent_tenant_type_uuid: Uuid::nil(),
                approver_uuid: Uuid::new_v4(),
                resolved_at: now,
            },
        )
        .await
        .expect_err("type-drift mid-apply must surface Validation");

    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("type changed under TX"),
                "expected TOCTOU diagnostic, got {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }

    // The pending row MUST still be `Pending`. Re-read via the
    // service repo path; soft-delete is also untouched.
    let row = conv_repo
        .find_by_id(&allow_all(), req_id)
        .await
        .expect("find_by_id")
        .expect("row exists");
    assert_eq!(row.status, ConversionStatus::Pending);
    assert!(row.approved_by.is_none());
    assert!(row.resolved_at.is_none());

    // `tenants.self_managed` MUST NOT have flipped despite the
    // apply having opened a TX. The drifted `tenant_type_uuid`
    // remains (the test mutated it deliberately), but the apply's
    // self_managed UPDATE was guarded by the same TOCTOU
    // predicate and never landed.
    let post_self_managed = fetch_tenant(&h.provider, child)
        .await
        .expect("fetch child post")
        .expect("child row")
        .self_managed;
    assert_eq!(
        post_self_managed, pre_self_managed,
        "tenants.self_managed MUST NOT flip when apply rolls back"
    );
    let post_barrier = fetch_closure_row(&h.provider, root, child)
        .await
        .expect("(root,child) closure read")
        .expect("(root,child) closure row")
        .barrier;
    assert_eq!(
        post_barrier, pre_barrier,
        "closure barrier MUST NOT change when apply rolls back"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approve_recomputes_barrier_for_closure_rows_referencing_soft_deleted_descendants() {
    // Tree: root -> mid -> leaf. All managed initially.
    // Soft-delete `leaf` (its closure rows are retained until
    // hard-delete). Approve a conversion converting `mid` to
    // self-managed. The `(root, leaf)` closure row's strict path
    // crosses `mid` and MUST be flipped to `barrier = 1`, even
    // though `leaf` is soft-deleted.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let mid = Uuid::new_v4();
    let leaf = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, mid, root, "mid", false, 1).await;
    create_active_child(&h, leaf, mid, "leaf", false, 2).await;

    let now = fixed_now();
    stamp_soft_deleted(&h.provider, leaf, now).await;

    let (svc, _conv_repo) = build_service(&h, now);

    let initiated = svc
        .request_conversion(
            &ctx_for(root),
            RequestConversionInput {
                tenant_id: mid,
                caller: ConversionCaller::child(mid),
                target_mode_override: None,
            },
        )
        .await
        .expect("request_conversion on mid");

    let approved = svc
        .approve(&ctx_for(root), initiated.id, ConversionCaller::parent(root))
        .await
        .expect("approve mid -> self_managed");
    assert_eq!(approved.status, ConversionStatus::Approved);

    // (root, mid): strict path = {mid}, mid is now self-managed -> barrier=1.
    let row_root_mid = fetch_closure_row(&h.provider, root, mid)
        .await
        .unwrap()
        .expect("(root, mid) row");
    assert_eq!(
        row_root_mid.barrier, 1,
        "(root, mid) barrier MUST flip to 1"
    );

    // (root, leaf): strict path = {mid, leaf}, mid is self-managed
    // even though leaf is soft-deleted -> barrier=1. This is the
    // load-bearing assertion: pre-fix, the barrier-recompute snapshot
    // excluded `leaf` from `parent_map`, so `strict_path_crosses_impl`
    // could not walk through it and the closure row was silently
    // skipped.
    let row_root_leaf = fetch_closure_row(&h.provider, root, leaf)
        .await
        .unwrap()
        .expect("(root, leaf) row retained until hard-delete");
    assert_eq!(
        row_root_leaf.barrier, 1,
        "(root, leaf) barrier MUST flip to 1 even though leaf is soft-deleted"
    );

    // (mid, leaf): strict path = {leaf}, no self-managed tenant on
    // it -> barrier stays 0 (path does NOT cross mid since mid is
    // the ancestor, excluded by the strict-path rule).
    let row_mid_leaf = fetch_closure_row(&h.provider, mid, leaf)
        .await
        .unwrap()
        .expect("(mid, leaf) row");
    assert_eq!(row_mid_leaf.barrier, 0);
}
