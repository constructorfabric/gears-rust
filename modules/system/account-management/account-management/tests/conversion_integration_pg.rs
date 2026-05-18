//! Real-Postgres integration tests for the conversion-request
//! lifecycle. Mirrors a deliberately small subset of the
//! `SQLite`-backed `conversion_integration.rs` cases — only the ones
//! that EXERCISE Postgres-specific behaviour the `SQLite` path
//! cannot:
//!
//! * **Single-pending partial-unique index.** Postgres uses a true
//!   partial index (`WHERE status = ... AND deleted_at IS NULL`)
//!   syntax that `SQLite` emulates with a different shape; pinning
//!   the PG-side collision keeps the partial-index DDL under test.
//! * **Real `SERIALIZABLE` snapshot isolation.** The dual-consent
//!   apply transaction runs under SERIALIZABLE and exercises the
//!   classifier ladder against a real PG `40001` envelope on
//!   contention. The happy-path approve below drives the un-contended
//!   SI path end-to-end against a real Postgres testcontainer.
//!
//! Gated behind `#[cfg(feature = "postgres")]` so the default
//! `cargo test` run does not require Docker. Enable explicitly:
//! `cargo test -p cf-account-management --features postgres
//!  --test conversion_integration_pg`.

#![cfg(feature = "postgres")]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration as StdDuration;

use account_management::domain::conversion::model::{
    ConversionSide, ConversionStatus, NewConversionRequest, TargetMode,
};
use account_management::domain::conversion::repo::ConversionRepo;
use account_management::domain::conversion::service::{
    ConversionCaller, ConversionService, RequestConversionInput,
};
use account_management::domain::error::DomainError;
use account_management::domain::tenant::TenantRepo;
use account_management::domain::tenant::closure::build_activation_rows;
use account_management::domain::tenant::model::{NewTenant, TenantStatus};
use account_management::domain::tenant_type::inert_tenant_type_checker;
use account_management::infra::storage::repo_impl::ConversionRepoImpl;
use time::{Duration as TimeDuration, OffsetDateTime};
use uuid::Uuid;

use common::pg::bring_up_postgres;
use common::*;

const APPROVAL_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch")
}

async fn create_active_child(
    h: &common::pg::PgHarness,
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

async fn seed_root(h: &common::pg::PgHarness, root_id: Uuid) {
    insert_tenant(&h.provider, root_id, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root");
    insert_closure(&h.provider, root_id, root_id, 0, ACTIVE)
        .await
        .expect("seed root self-row");
}

// ---------------------------------------------------------------------
// 1. Happy-path approve roundtrip on real Postgres.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_happy_path_approve_flips_self_managed_and_barrier() {
    let h = bring_up_postgres()
        .await
        .expect("postgres testcontainer (Docker daemon required)");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await;

    let conv_repo = Arc::new(ConversionRepoImpl::new(Arc::clone(&h.provider)));
    let conv_repo_dyn: Arc<dyn ConversionRepo> = conv_repo.clone();
    let tenant_repo: Arc<dyn TenantRepo> = h.repo.clone();
    let now = fixed_now();
    let now_fn = Arc::new(move || now);
    let svc = Arc::new(
        ConversionService::new(
            conv_repo_dyn,
            tenant_repo,
            inert_tenant_type_checker(),
            mock_enforcer(),
            StdDuration::from_secs(APPROVAL_TTL_SECS),
            StdDuration::from_secs(RETENTION_SECS),
        )
        .with_now_fn(now_fn),
    );

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

    let approved = svc
        .approve(&ctx_for(root), initiated.id, ConversionCaller::parent(root))
        .await
        .expect("approve");
    assert_eq!(approved.status, ConversionStatus::Approved);

    // tenants.self_managed flipped.
    let row = fetch_tenant(&h.provider, child)
        .await
        .unwrap()
        .expect("tenant row");
    assert!(row.self_managed);

    // (root, child) closure barrier = 1.
    let strict = fetch_closure_row(&h.provider, root, child)
        .await
        .unwrap()
        .expect("(root, child) row");
    assert_eq!(strict.barrier, 1);
    let _ = TimeDuration::ZERO;
}

// ---------------------------------------------------------------------
// 2. Single-pending partial-unique index on Postgres.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_single_pending_unique_index_rejects_second_insert() {
    let h = bring_up_postgres().await.expect("postgres");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await;

    let conv_repo = Arc::new(ConversionRepoImpl::new(Arc::clone(&h.provider)));

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

// ---------------------------------------------------------------------
// 3. Concurrent approve contention drives the SERIALIZABLE retry path.
// ---------------------------------------------------------------------
//
// Two concurrent counterparty approves race against the same pending
// row. Under real Postgres SERIALIZABLE, exactly one of them MUST land
// `Approved` and the other MUST surface `AlreadyResolved`. The
// `with_serializable_retry` helper either:
//   * catches the `40001` SQLSTATE on commit, retries, and the retry
//     re-loads the row, observes `status != Pending`, and returns
//     `AlreadyResolved`; or
//   * the second writer's stamp UPDATE finds zero matching rows on the
//     fence (`status = Pending AND deleted_at IS NULL`) and surfaces
//     `AlreadyResolved` directly (the B1 row-affected guard).
//
// Either way the externally observable outcome is the same: one Ok, one
// `AlreadyResolved`. Pinning this with a real PG round-trip is the
// reason this file exists.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_concurrent_approves_serialize_to_one_winner() {
    let h = bring_up_postgres()
        .await
        .expect("postgres testcontainer (Docker daemon required)");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root(&h, root).await;
    create_active_child(&h, child, root, "c", false, 1).await;

    let conv_repo = Arc::new(ConversionRepoImpl::new(Arc::clone(&h.provider)));
    let conv_repo_dyn: Arc<dyn ConversionRepo> = conv_repo.clone();
    let tenant_repo: Arc<dyn TenantRepo> = h.repo.clone();
    let now = fixed_now();
    let now_fn = Arc::new(move || now);
    let svc = Arc::new(
        ConversionService::new(
            conv_repo_dyn,
            tenant_repo,
            inert_tenant_type_checker(),
            mock_enforcer(),
            StdDuration::from_secs(APPROVAL_TTL_SECS),
            StdDuration::from_secs(RETENTION_SECS),
        )
        .with_now_fn(now_fn),
    );

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

    // Two parallel counterparty approves against the same pending row.
    // Both tasks share the same `ctx_for(root)` actor (sourced from
    // `SecurityContext::subject_id`); the winner is established by the
    // SERIALIZABLE retry classifier, not by a distinguishing approver
    // uuid.
    //
    // A `Barrier::new(3)` gates the two approve calls AND the main
    // task on a single rendezvous so both transactions begin inside
    // each other's MVCC snapshot. Without the gate `tokio::spawn`
    // only queues the work — task A could complete its SERIALIZABLE
    // commit before task B is polled, and the test would pass via
    // the B1 fence (`AlreadyResolved` from the in-TX re-read)
    // rather than via the `40001` serialization-failure classifier
    // the test claims to exercise. With the gate the two
    // `apply_conversion_approval` calls strictly overlap, forcing
    // the SERIALIZABLE conflict that the classifier was built to
    // map to `AlreadyResolved` on retry exhaustion.
    let gate = Arc::new(tokio::sync::Barrier::new(3));
    let svc_a = Arc::clone(&svc);
    let svc_b = Arc::clone(&svc);
    let gate_a = Arc::clone(&gate);
    let gate_b = Arc::clone(&gate);
    let id = initiated.id;
    let task_a = tokio::spawn(async move {
        gate_a.wait().await;
        svc_a
            .approve(&ctx_for(root), id, ConversionCaller::parent(root))
            .await
    });
    let task_b = tokio::spawn(async move {
        gate_b.wait().await;
        svc_b
            .approve(&ctx_for(root), id, ConversionCaller::parent(root))
            .await
    });
    gate.wait().await;

    let res_a = task_a.await.expect("task A join");
    let res_b = task_b.await.expect("task B join");

    let oks = [&res_a, &res_b].iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        oks, 1,
        "exactly one approve must win; got A={res_a:?} B={res_b:?}"
    );

    // The losing task MUST surface `AlreadyResolved` (and nothing
    // else — not Validation, not Conflict, not Internal). This is the
    // contract the B1 fence + the SERIALIZABLE retry classifier
    // promises to the caller.
    let ((Err(losing_err), Ok(_)) | (Ok(_), Err(losing_err))) = (&res_a, &res_b) else {
        panic!("inconsistent outcome A={res_a:?} B={res_b:?}");
    };
    assert!(
        matches!(losing_err, DomainError::AlreadyResolved),
        "loser must surface AlreadyResolved, got {losing_err:?}"
    );

    // Final DB state: tenant flipped exactly once, closure barrier
    // recomputed exactly once. Both invariants hold regardless of
    // which task won the race.
    let row = fetch_tenant(&h.provider, child)
        .await
        .unwrap()
        .expect("tenant row");
    assert!(
        row.self_managed,
        "tenant must be self-managed after winner approve"
    );
    let strict = fetch_closure_row(&h.provider, root, child)
        .await
        .unwrap()
        .expect("(root, child) row");
    assert_eq!(strict.barrier, 1, "barrier must reflect the post-flip view");
    let _ = TimeDuration::ZERO;
}
