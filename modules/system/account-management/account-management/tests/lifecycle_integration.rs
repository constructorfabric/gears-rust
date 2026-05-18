//! Real-DB integration tests for the tenant-lifecycle write paths
//! (`insert_provisioning`, `activate_tenant`, `update_tenant_mutable`,
//! `schedule_deletion`, `compensate_provisioning`) against in-memory
//! `SQLite`.
//!
//! These tests exercise the production `TenantRepoImpl` saga
//! transitions and verify closure-table invariants the read-side
//! integrity checker relies on:
//!
//! * Provisioning rows MUST NOT have `tenant_closure` rows
//!   (ADR-0007).
//! * `activate_tenant` writes the self-row plus one row per strict
//!   ancestor (`build_activation_rows` shape).
//! * `update_tenant_mutable` status flip rewrites
//!   `descendant_status` for every closure row pointing at the
//!   tenant in the same tx (DESIGN §3.1 closure status
//!   denormalization invariant).
//! * `schedule_deletion` marks the tenant `Deleted`, stamps
//!   `deleted_at` (which also starts the retention timer), and
//!   rewrites closure `descendant_status` in one tx.
//! * `compensate_provisioning` deletes the `Provisioning` row
//!   without touching closure (no closure ever existed).

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::time::Duration;

use account_management::domain::tenant::TenantRepo;
use account_management::domain::tenant::closure::build_activation_rows;
use account_management::domain::tenant::model::{NewTenant, TenantStatus};
use account_management::domain::tenant::retention::{HardDeleteEligibility, HardDeleteOutcome};
use time::OffsetDateTime;
use uuid::Uuid;

use common::*;

fn tenant_type_uuid() -> Uuid {
    Uuid::from_u128(0xAA)
}

/// Seed the platform root tenant directly. Used by tests that want a
/// pre-existing root without driving the bootstrap saga.
/// `insert_provisioning(parent_id = None)` is also valid in
/// production now (the root path skips the parent-active fence and
/// relies on the schema's single-root invariant); see
/// `insert_provisioning_root_path_succeeds_without_parent` below for
/// the production-path coverage.
async fn seed_root_directly(h: &Harness, root_id: Uuid) {
    insert_tenant(&h.provider, root_id, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root tenant");
    insert_closure(&h.provider, root_id, root_id, 0, ACTIVE)
        .await
        .expect("seed root self-row");
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
        tenant_type_uuid: tenant_type_uuid(),
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

// ---------------------------------------------------------------------
// Saga step 1 — root path: `insert_provisioning(parent_id = None)`
// writes a `Provisioning` root row without touching `tenant_closure`,
// and the schema-level partial unique index `ux_tenants_single_root`
// rejects a second root insert. Production-path coverage for the
// platform-bootstrap saga's `insert_root_provisioning` call.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_provisioning_root_path_succeeds_without_parent() {
    let h = setup_sqlite().await.expect("sqlite");
    let root_id = Uuid::new_v4();
    let new = NewTenant {
        id: root_id,
        parent_id: None,
        name: "platform-root".into(),
        self_managed: false,
        tenant_type_uuid: tenant_type_uuid(),
        depth: 0,
    };
    h.repo
        .insert_provisioning(&allow_all(), &new)
        .await
        .expect("root insert_provisioning");

    let row = fetch_tenant(&h.provider, root_id)
        .await
        .unwrap()
        .expect("root row exists");
    assert_eq!(row.parent_id, None, "root parent_id must be NULL");
    assert_eq!(row.depth, 0, "root depth must be 0");
    assert_eq!(row.status, PROVISIONING);

    // ADR-0007: provisioning rows have no closure rows.
    assert!(
        fetch_closure_row(&h.provider, root_id, root_id)
            .await
            .unwrap()
            .is_none(),
        "root provisioning row MUST NOT have a self-row"
    );

    // Schema-level single-root invariant: a second `parent_id = NULL`
    // insert must be rejected by `ux_tenants_single_root`.
    let new2 = NewTenant {
        id: Uuid::new_v4(),
        parent_id: None,
        name: "second-root".into(),
        self_managed: false,
        tenant_type_uuid: tenant_type_uuid(),
        depth: 0,
    };
    // The repo's unique-violation classifier maps the
    // `ux_tenants_single_root` collision to `DomainError::AlreadyExists`.
    let err = h
        .repo
        .insert_provisioning(&allow_all(), &new2)
        .await
        .expect_err("second root insert must be rejected by ux_tenants_single_root");
    assert!(
        matches!(
            err,
            account_management::domain::error::DomainError::AlreadyExists { .. }
        ),
        "expected DomainError::AlreadyExists from ux_tenants_single_root collision, got: {err:?}"
    );
}

// ---------------------------------------------------------------------
// Saga step 1 — `insert_provisioning` does NOT touch tenant_closure.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_provisioning_does_not_write_closure_rows() {
    let h = setup_sqlite().await.expect("sqlite");
    let root_id = Uuid::new_v4();
    seed_root_directly(&h, root_id).await;

    let child_id = Uuid::new_v4();
    let new = NewTenant {
        id: child_id,
        parent_id: Some(root_id),
        name: "child".into(),
        self_managed: false,
        tenant_type_uuid: tenant_type_uuid(),
        depth: 1,
    };
    h.repo
        .insert_provisioning(&allow_all(), &new)
        .await
        .expect("insert_provisioning");

    // ADR-0007: Provisioning rows MUST NOT have closure rows. Both
    // self-row and any (root, child) edge MUST be absent until
    // `activate_tenant` runs.
    assert!(
        fetch_closure_row(&h.provider, child_id, child_id)
            .await
            .unwrap()
            .is_none(),
        "Provisioning row must NOT have a self-row"
    );
    assert!(
        fetch_closure_row(&h.provider, root_id, child_id)
            .await
            .unwrap()
            .is_none(),
        "Provisioning row must NOT have a (parent, child) edge"
    );

    // The row exists in `tenants` with status = Provisioning.
    let row = fetch_tenant(&h.provider, child_id)
        .await
        .unwrap()
        .expect("provisioning row exists");
    assert_eq!(row.status, PROVISIONING);
}

// ---------------------------------------------------------------------
// Saga step 1 — depth-fence negative tests.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_provisioning_rejects_root_with_non_zero_depth() {
    let h = setup_sqlite().await.expect("sqlite");
    let err = h
        .repo
        .insert_provisioning(
            &allow_all(),
            &NewTenant {
                id: Uuid::new_v4(),
                parent_id: None,
                name: "bad-root".into(),
                self_managed: false,
                tenant_type_uuid: tenant_type_uuid(),
                depth: 1,
            },
        )
        .await
        .expect_err("non-zero root depth must be rejected");
    assert!(
        matches!(
            err,
            account_management::domain::error::DomainError::Validation { .. }
        ),
        "expected DomainError::Validation, got: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_provisioning_rejects_child_depth_mismatch() {
    let h = setup_sqlite().await.expect("sqlite");
    let root_id = Uuid::new_v4();
    seed_root_directly(&h, root_id).await;

    let err = h
        .repo
        .insert_provisioning(
            &allow_all(),
            &NewTenant {
                id: Uuid::new_v4(),
                parent_id: Some(root_id),
                name: "bad-child".into(),
                self_managed: false,
                tenant_type_uuid: tenant_type_uuid(),
                depth: 5,
            },
        )
        .await
        .expect_err("depth mismatch must be rejected");
    assert!(
        matches!(
            err,
            account_management::domain::error::DomainError::Validation { .. }
        ),
        "expected DomainError::Validation, got: {err:?}"
    );
}

// ---------------------------------------------------------------------
// Saga step 3 — `activate_tenant` writes self-row + strict-ancestor
// edges.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_tenant_writes_full_closure_for_two_node_chain() {
    let h = setup_sqlite().await.expect("sqlite");
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    seed_root_directly(&h, root_id).await;
    create_active_child(&h, child_id, root_id, "child", false, 1).await;

    // Self-row for child.
    let child_self = fetch_closure_row(&h.provider, child_id, child_id)
        .await
        .unwrap()
        .expect("child self-row");
    assert_eq!(child_self.barrier, 0);
    assert_eq!(child_self.descendant_status, ACTIVE);

    // Strict (root, child) edge.
    let strict = fetch_closure_row(&h.provider, root_id, child_id)
        .await
        .unwrap()
        .expect("strict (root, child) edge");
    assert_eq!(strict.barrier, 0);
    assert_eq!(strict.descendant_status, ACTIVE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_tenant_writes_full_closure_for_three_node_chain_with_self_managed() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let mid = Uuid::new_v4();
    let leaf = Uuid::new_v4();
    seed_root_directly(&h, root).await;
    create_active_child(&h, mid, root, "mid", true, 1).await; // self_managed
    create_active_child(&h, leaf, mid, "leaf", false, 2).await;

    // Per planner barrier semantics: `(A, D]` strict path determines
    // barrier. For (mid, leaf): path = {leaf}, barrier=0. For (root,
    // leaf): path = {mid, leaf}, mid is self_managed → barrier=1.
    // For (root, mid): path = {mid}, mid is self_managed → barrier=1.
    let edge_root_mid = fetch_closure_row(&h.provider, root, mid)
        .await
        .unwrap()
        .expect("(root, mid)");
    assert_eq!(edge_root_mid.barrier, 1);

    let edge_root_leaf = fetch_closure_row(&h.provider, root, leaf)
        .await
        .unwrap()
        .expect("(root, leaf)");
    assert_eq!(edge_root_leaf.barrier, 1);

    let edge_mid_leaf = fetch_closure_row(&h.provider, mid, leaf)
        .await
        .unwrap()
        .expect("(mid, leaf)");
    assert_eq!(edge_mid_leaf.barrier, 0);
}

// ---------------------------------------------------------------------
// `TenantRepo::set_status` status flip — Active -> Suspended →
// rewrites every closure row's `descendant_status` for the tenant
// in the same tx.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_status_flip_rewrites_closure_descendant_status() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let mid = Uuid::new_v4();
    let leaf = Uuid::new_v4();
    seed_root_directly(&h, root).await;
    create_active_child(&h, mid, root, "mid", false, 1).await;
    create_active_child(&h, leaf, mid, "leaf", false, 2).await;

    // Sanity: every row pointing at `mid` is Active.
    for row in fetch_closure_rows_for_descendant(&h.provider, mid)
        .await
        .unwrap()
    {
        assert_eq!(row.descendant_status, ACTIVE);
    }

    // Flip mid -> Suspended via the dedicated repo method.
    h.repo
        .set_status(
            &allow_all(),
            mid,
            TenantStatus::Suspended,
            time::OffsetDateTime::now_utc(),
        )
        .await
        .expect("set_status mid -> Suspended");

    // tenants.status updated.
    let updated = fetch_tenant(&h.provider, mid)
        .await
        .unwrap()
        .expect("tenant row");
    assert_eq!(updated.status, SUSPENDED);

    // Every closure row pointing at `mid` now carries Suspended.
    let rows = fetch_closure_rows_for_descendant(&h.provider, mid)
        .await
        .unwrap();
    assert!(!rows.is_empty(), "mid must have closure rows");
    for row in &rows {
        assert_eq!(
            row.descendant_status, SUSPENDED,
            "closure row {row:?} must carry Suspended after status flip"
        );
    }

    // Closure rows for `leaf` (descendant of `mid`) are NOT cascaded:
    // suspending `mid` does not suspend `leaf` per DESIGN §3.1
    // (status changes are non-cascading).
    for row in fetch_closure_rows_for_descendant(&h.provider, leaf)
        .await
        .unwrap()
    {
        assert_eq!(
            row.descendant_status, ACTIVE,
            "leaf descendant_status must NOT cascade from mid -> Suspended"
        );
    }

    // Round-trip back to Active.
    h.repo
        .set_status(
            &allow_all(),
            mid,
            TenantStatus::Active,
            time::OffsetDateTime::now_utc(),
        )
        .await
        .expect("set_status mid -> Active");
    for row in fetch_closure_rows_for_descendant(&h.provider, mid)
        .await
        .unwrap()
    {
        assert_eq!(row.descendant_status, ACTIVE);
    }
}

// ---------------------------------------------------------------------
// `schedule_deletion` — soft-delete marks tenant Deleted, stamps
// `deleted_at` (retention-timer start), and rewrites closure
// `descendant_status` to Deleted in the same tx.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_deletion_rewrites_closure_to_deleted() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let leaf = Uuid::new_v4();
    seed_root_directly(&h, root).await;
    create_active_child(&h, leaf, root, "leaf", false, 1).await;

    let now = OffsetDateTime::now_utc();
    let outcome = h
        .repo
        .schedule_deletion(&allow_all(), leaf, now, None)
        .await
        .expect("schedule_deletion");
    assert_eq!(outcome.status, TenantStatus::Deleted);
    assert!(outcome.deleted_at.is_some());

    // Closure rows for `leaf` carry Deleted; the row itself remains
    // (closure cleanup is hard-delete's job, not soft-delete's).
    let rows = fetch_closure_rows_for_descendant(&h.provider, leaf)
        .await
        .unwrap();
    assert!(!rows.is_empty(), "soft-delete preserves closure rows");
    for row in &rows {
        assert_eq!(
            row.descendant_status, DELETED,
            "soft-delete must rewrite descendant_status to Deleted: {row:?}"
        );
    }

    // tenants row is still present (just status-flipped + stamped).
    let tenants_after = fetch_all_tenant_ids(&h.provider).await.unwrap();
    assert!(
        tenants_after.contains(&leaf),
        "soft-delete preserves the tenants row"
    );
}

// ---------------------------------------------------------------------
// `compensate_provisioning` — deletes a `Provisioning` row that
// never reached activation. No closure cleanup needed because
// `insert_provisioning` never wrote any.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compensate_provisioning_deletes_provisioning_row() {
    let h = setup_sqlite().await.expect("sqlite");
    let root_id = Uuid::new_v4();
    seed_root_directly(&h, root_id).await;

    let stuck = Uuid::new_v4();
    let new = NewTenant {
        id: stuck,
        parent_id: Some(root_id),
        name: "stuck".into(),
        self_managed: false,
        tenant_type_uuid: tenant_type_uuid(),
        depth: 1,
    };
    h.repo
        .insert_provisioning(&allow_all(), &new)
        .await
        .expect("insert_provisioning");

    // Saga compensation path — `expected_claimed_by = None` (the
    // saga holds the row exclusively pre-IdP; no reaper claim yet).
    h.repo
        .compensate_provisioning(&allow_all(), stuck, None)
        .await
        .expect("compensate_provisioning");

    // The stuck row is gone; root is preserved.
    let surviving = fetch_all_tenant_ids(&h.provider).await.unwrap();
    assert!(
        !surviving.contains(&stuck),
        "compensated row must be deleted"
    );
    assert!(
        surviving.contains(&root_id),
        "unrelated root must be preserved"
    );

    // No closure rows reference the compensated tenant.
    assert!(
        fetch_closure_rows_referencing(&h.provider, stuck)
            .await
            .unwrap()
            .is_empty(),
        "compensated row never had closure rows in the first place"
    );
}

// ---------------------------------------------------------------------
// Cross-cutting: a fully bootstrapped two-node tree comes out clean
// under the integrity check (closure invariants + status denorm
// hold immediately after the saga).
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saga_built_tree_passes_integrity_check() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    seed_root_directly(&h, root).await;
    create_active_child(&h, child, root, "child", false, 1).await;

    let viols = h
        .repo
        .run_integrity_check(&allow_all())
        .await
        .expect("integrity check");
    assert!(
        viols.is_empty(),
        "saga-built tree must produce zero integrity violations: {viols:?}"
    );
}

// ---------------------------------------------------------------------
// `hard_delete_one` + `check_hard_delete_eligibility` end-to-end:
//
//   schedule_deletion
//     -> manually stamp `claimed_by` (see note on
//        `stamp_retention_claim` below)
//     -> check_hard_delete_eligibility -> Eligible
//     -> hard_delete_one             -> Cleaned
//
// Asserts the reclaim contract: post-`hard_delete_one`, the
// `tenants` row is gone and every `tenant_closure` row referencing
// the tenant (as ancestor or descendant) is gone too. Both are
// invariants the integrity classifier later relies on — a residual
// closure row for a tenant the `tenants` table no longer holds is
// the canonical `StaleClosureRow` shape, and the hard-delete path
// must NEVER produce it.
//
// The leaf is targeted (not the root) on purpose: `hard_delete_one`
// defers any tenant with live children via
// `HardDeleteOutcome::DeferredChildPresent`, and exercising the
// `Cleaned` outcome requires a row with no descendants in `tenants`.
//
// The `claimed_by` stamp is done directly via the harness rather
// than going through `scan_retention_due` because the SQLite path
// of that scanner currently surfaces zero rows (a separately-
// tracked production-side issue around `Expr::cust_with_values`
// `$N` placeholder rewriting on the SQLite backend — Postgres is
// unaffected). End-to-end retention-pipeline coverage that exercises
// `scan_retention_due` itself is folded into the Postgres real-DB
// suite as a follow-up; the in-tx contract this test pins
// (`Eligible -> Cleaned -> closure cleanup`) is what
// `hard_delete_one` owns and is fully covered here.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_delete_one_cleans_tenant_and_closure_rows() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let leaf = Uuid::new_v4();
    seed_root_directly(&h, root).await;
    create_active_child(&h, leaf, root, "leaf", false, 1).await;

    // Soft-delete the leaf so it enters the `Deleted` state with
    // `deleted_at` stamped — the structural precondition both
    // `check_hard_delete_eligibility` and `hard_delete_one` verify.
    let scheduled_at = OffsetDateTime::now_utc();
    h.repo
        .schedule_deletion(
            &allow_all(),
            leaf,
            scheduled_at,
            Some(Duration::from_secs(1)),
        )
        .await
        .expect("schedule_deletion");

    let after_soft_delete = fetch_tenant(&h.provider, leaf)
        .await
        .unwrap()
        .expect("leaf row present after soft-delete");
    assert_eq!(after_soft_delete.status, DELETED);
    assert!(after_soft_delete.deleted_at.is_some());

    // Stamp `claimed_by` / `claimed_at` directly to mimic what the
    // retention-scan claim UPDATE does on a healthy backend, then
    // exercise the eligibility preflight + hard-delete in tx.
    let worker_id = Uuid::new_v4();
    stamp_retention_claim(&h.provider, leaf, worker_id, scheduled_at)
        .await
        .expect("stamp retention claim");

    let eligibility = h
        .repo
        .check_hard_delete_eligibility(&allow_all(), leaf, worker_id)
        .await
        .expect("check_hard_delete_eligibility");
    assert_eq!(eligibility, HardDeleteEligibility::Eligible);

    let outcome = h
        .repo
        .hard_delete_one(&allow_all(), leaf, worker_id)
        .await
        .expect("hard_delete_one");
    assert_eq!(outcome, HardDeleteOutcome::Cleaned);

    assert!(
        fetch_tenant(&h.provider, leaf).await.unwrap().is_none(),
        "hard-deleted tenant row must be absent from `tenants`"
    );
    assert!(
        fetch_closure_rows_referencing(&h.provider, leaf)
            .await
            .unwrap()
            .is_empty(),
        "all `tenant_closure` rows referencing the hard-deleted leaf must be gone"
    );

    let surviving = fetch_all_tenant_ids(&h.provider).await.unwrap();
    assert!(
        surviving.contains(&root),
        "root tenant must survive the leaf's hard-delete"
    );
    assert!(
        !surviving.contains(&leaf),
        "leaf tenant must not survive hard-delete"
    );
}

// ---------------------------------------------------------------------
// Lost-claim guard — `hard_delete_one` returns `NotEligible` when the
// supplied `claimed_by` does not match the row's stamped claim.
//
// Pins the `RETENTION_CLAIM_TTL`-busting peer-takeover scenario from
// the trait docstring on `TenantRepo::hard_delete_one`: if a worker's
// hooks + `IdP` round-trip exceeds the claim window and a peer
// reaper re-claims the row, the original worker MUST refuse to
// proceed (else both workers race the DB teardown).
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_delete_one_rejects_lost_claim() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    let leaf = Uuid::new_v4();
    seed_root_directly(&h, root).await;
    create_active_child(&h, leaf, root, "leaf", false, 1).await;

    let scheduled_at = OffsetDateTime::now_utc();
    h.repo
        .schedule_deletion(
            &allow_all(),
            leaf,
            scheduled_at,
            Some(Duration::from_secs(1)),
        )
        .await
        .expect("schedule_deletion");

    // Peer reaper holds the claim.
    let peer_worker = Uuid::new_v4();
    stamp_retention_claim(&h.provider, leaf, peer_worker, scheduled_at)
        .await
        .expect("stamp retention claim under peer worker id");

    // Original worker tries to finalize with a stale token.
    let stale_worker = Uuid::new_v4();
    let outcome = h
        .repo
        .hard_delete_one(&allow_all(), leaf, stale_worker)
        .await
        .expect("hard_delete_one");
    assert_eq!(outcome, HardDeleteOutcome::NotEligible);

    // The leaf must still exist — neither the tenant row nor its
    // closure rows are touched on a lost-claim refusal.
    assert!(
        fetch_tenant(&h.provider, leaf).await.unwrap().is_some(),
        "lost-claim refusal must NOT delete the tenant row"
    );
    assert!(
        !fetch_closure_rows_referencing(&h.provider, leaf)
            .await
            .unwrap()
            .is_empty(),
        "lost-claim refusal must NOT delete closure rows"
    );
}
