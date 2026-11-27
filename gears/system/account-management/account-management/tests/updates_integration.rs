//! Integration tests for the `TenantRepoImpl` update paths in
//! `repo_impl/updates.rs` — `update_tenant_mutable`, `set_status`,
//! `erase_tenant_name`, `schedule_deletion`, and
//! `load_ancestor_chain_through_parent`.
//!
//! `lifecycle_integration.rs` covers the happy status-flip / soft-delete
//! paths; this suite targets the **guard and edge branches** those
//! happy-path tests skip: not-found, status-conflict rejections,
//! idempotent no-ops, the closure self-row-missing invariant, and the
//! ancestor-chain resolver's fallback + fail-closed branches.
//!
//! All multi-row fixtures seed at most one root (depth 0, NULL parent)
//! to satisfy `ux_tenants_single_root` / `ck_tenants_root_depth`;
//! `SQLite` has foreign keys disabled so dangling `parent_id`s are
//! accepted.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use account_management::domain::error::DomainError;
use account_management::domain::tenant::TenantRepo;
use account_management::domain::tenant::model::TenantStatus;
use account_management_sdk::UpdateTenantRequest;
use time::OffsetDateTime;
use uuid::Uuid;

use common::*;

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

// =====================================================================
// update_tenant_mutable
// =====================================================================

#[tokio::test]
async fn update_tenant_mutable_not_found() {
    let h = setup_sqlite().await.expect("setup");
    let err = h
        .repo
        .update_tenant_mutable(
            &allow_all(),
            Uuid::new_v4(),
            &UpdateTenantRequest::new().with_name("x"),
        )
        .await
        .expect_err("missing tenant must error");
    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn update_tenant_mutable_rejects_deleted_and_provisioning() {
    let h = setup_sqlite().await.expect("setup");
    let del = Uuid::new_v4();
    let prov = Uuid::new_v4();
    insert_deleted_tenant(
        &h.provider,
        del,
        None,
        "del",
        0,
        now(),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("seed deleted root");
    insert_provisioning_tenant(
        &h.provider,
        prov,
        Some(del),
        "prov",
        1,
        now(),
        None,
        None,
        None,
    )
    .await
    .expect("seed provisioning");

    let patch = UpdateTenantRequest::new().with_name("new");
    let e1 = h
        .repo
        .update_tenant_mutable(&allow_all(), del, &patch)
        .await
        .expect_err("deleted not mutable");
    assert!(matches!(e1, DomainError::Conflict { .. }), "got {e1:?}");

    let e2 = h
        .repo
        .update_tenant_mutable(&allow_all(), prov, &patch)
        .await
        .expect_err("provisioning not mutable");
    assert!(matches!(e2, DomainError::Conflict { .. }), "got {e2:?}");
}

#[tokio::test]
async fn update_tenant_mutable_noop_when_name_unchanged() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    insert_tenant(&h.provider, id, None, "same", ACTIVE, false, 0)
        .await
        .expect("seed");
    let before = fetch_tenant(&h.provider, id).await.unwrap().unwrap();

    let out = h
        .repo
        .update_tenant_mutable(
            &allow_all(),
            id,
            &UpdateTenantRequest::new().with_name("same"),
        )
        .await
        .expect("noop patch");
    assert_eq!(out.name, "same");

    let after = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
    assert_eq!(
        before.updated_at, after.updated_at,
        "no-op patch must not bump updated_at"
    );
}

#[tokio::test]
async fn update_tenant_mutable_renames() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    insert_tenant(&h.provider, id, None, "old", ACTIVE, false, 0)
        .await
        .expect("seed");

    let out = h
        .repo
        .update_tenant_mutable(
            &allow_all(),
            id,
            &UpdateTenantRequest::new().with_name("renamed"),
        )
        .await
        .expect("rename");
    assert_eq!(out.name, "renamed");
    let row = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
    assert_eq!(row.name, "renamed");
}

// =====================================================================
// erase_tenant_name
// =====================================================================

#[tokio::test]
async fn erase_tenant_name_not_found() {
    let h = setup_sqlite().await.expect("setup");
    let err = h
        .repo
        .erase_tenant_name(&allow_all(), Uuid::new_v4(), now())
        .await
        .expect_err("missing tenant must error");
    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn erase_tenant_name_rejects_every_status_but_deleted() {
    let h = setup_sqlite().await.expect("setup");
    let root = Uuid::new_v4();
    insert_tenant(&h.provider, root, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root");
    let suspended = Uuid::new_v4();
    insert_tenant(
        &h.provider,
        suspended,
        Some(root),
        "susp",
        SUSPENDED,
        false,
        1,
    )
    .await
    .expect("seed suspended");
    let provisioning = Uuid::new_v4();
    insert_provisioning_tenant(
        &h.provider,
        provisioning,
        Some(root),
        "prov",
        1,
        now(),
        None,
        None,
        None,
    )
    .await
    .expect("seed provisioning");

    for (id, name) in [(root, "root"), (suspended, "susp"), (provisioning, "prov")] {
        let err = h
            .repo
            .erase_tenant_name(&allow_all(), id, now())
            .await
            .expect_err("only a deleted tenant can be erased");
        assert!(
            matches!(err, DomainError::Conflict { .. }),
            "{name}: got {err:?}"
        );
        let row = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
        assert_eq!(row.name, name, "{name}: the name is untouched");
    }
}

#[tokio::test]
async fn erase_tenant_name_replaces_the_name_with_the_id_and_nothing_else() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    let deleted_at = now() - time::Duration::hours(5);
    insert_deleted_tenant(
        &h.provider,
        id,
        None,
        "Jane Doe Consulting",
        0,
        deleted_at,
        Some(120),
        None,
        None,
        None,
    )
    .await
    .expect("seed tombstone");
    let before = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
    let erased_at = before.updated_at + time::Duration::seconds(1);

    let out = h
        .repo
        .erase_tenant_name(&allow_all(), id, erased_at)
        .await
        .expect("erase");

    assert_eq!(out.name, id.to_string());
    assert_eq!(out.status, TenantStatus::Deleted);
    let after = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
    assert_eq!(after.name, id.to_string());
    assert_eq!(after.status, DELETED);
    assert_eq!(after.updated_at, erased_at);
    assert_eq!(
        after.deleted_at.map(OffsetDateTime::unix_timestamp),
        Some(deleted_at.unix_timestamp()),
        "the deletion instant is kept"
    );
    assert_eq!(
        after.retention_window_secs,
        Some(120),
        "the retention window is kept"
    );
}

#[tokio::test]
async fn erase_tenant_name_on_an_erased_tenant_is_a_noop() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    insert_deleted_tenant(
        &h.provider,
        id,
        None,
        "name",
        0,
        now(),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("seed tombstone");
    h.repo
        .erase_tenant_name(&allow_all(), id, now())
        .await
        .expect("first erase");
    let before = fetch_tenant(&h.provider, id).await.unwrap().unwrap();

    let out = h
        .repo
        .erase_tenant_name(&allow_all(), id, now() + time::Duration::hours(1))
        .await
        .expect("second erase");

    assert_eq!(out.name, id.to_string());
    let after = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
    assert_eq!(
        before.updated_at, after.updated_at,
        "a second erasure must not bump updated_at"
    );
}

// =====================================================================
// set_status
// =====================================================================

#[tokio::test]
async fn set_status_rejects_inadmissible_targets() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    insert_tenant(&h.provider, id, None, "t", ACTIVE, false, 0)
        .await
        .expect("seed");

    for target in [TenantStatus::Deleted, TenantStatus::Provisioning] {
        let err = h
            .repo
            .set_status(&allow_all(), id, target, now())
            .await
            .expect_err("inadmissible target");
        assert!(
            matches!(err, DomainError::Conflict { .. }),
            "target {target:?}: {err:?}"
        );
    }
}

#[tokio::test]
async fn set_status_not_found() {
    let h = setup_sqlite().await.expect("setup");
    let err = h
        .repo
        .set_status(&allow_all(), Uuid::new_v4(), TenantStatus::Suspended, now())
        .await
        .expect_err("missing");
    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn set_status_rejects_deleted_and_provisioning_current_state() {
    let h = setup_sqlite().await.expect("setup");
    let del = Uuid::new_v4();
    let prov = Uuid::new_v4();
    insert_deleted_tenant(
        &h.provider,
        del,
        None,
        "del",
        0,
        now(),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("seed deleted");
    insert_provisioning_tenant(
        &h.provider,
        prov,
        Some(del),
        "prov",
        1,
        now(),
        None,
        None,
        None,
    )
    .await
    .expect("seed provisioning");

    let e1 = h
        .repo
        .set_status(&allow_all(), del, TenantStatus::Active, now())
        .await
        .expect_err("deleted is terminal");
    assert!(matches!(e1, DomainError::Conflict { .. }), "got {e1:?}");

    let e2 = h
        .repo
        .set_status(&allow_all(), prov, TenantStatus::Active, now())
        .await
        .expect_err("provisioning not transitionable");
    assert!(matches!(e2, DomainError::Conflict { .. }), "got {e2:?}");
}

#[tokio::test]
async fn set_status_same_to_same_is_noop() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    insert_tenant(&h.provider, id, None, "t", ACTIVE, false, 0)
        .await
        .expect("seed");
    let before = fetch_tenant(&h.provider, id).await.unwrap().unwrap();

    let out = h
        .repo
        .set_status(&allow_all(), id, TenantStatus::Active, now())
        .await
        .expect("same-to-same");
    assert_eq!(out.status, TenantStatus::Active);
    let after = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
    assert_eq!(
        before.updated_at, after.updated_at,
        "no-op must not bump updated_at"
    );
}

#[tokio::test]
async fn set_status_fails_when_closure_self_row_missing() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    // Active tenant WITHOUT a `(id, id)` closure self-row: the status
    // flip must roll back with Internal (invariant breach), not leave
    // tenant_closure stale.
    insert_tenant(&h.provider, id, None, "no-closure", ACTIVE, false, 0)
        .await
        .expect("seed");

    let err = h
        .repo
        .set_status(&allow_all(), id, TenantStatus::Suspended, now())
        .await
        .expect_err("missing self-row must fail the tx");
    assert!(matches!(err, DomainError::Internal { .. }), "got {err:?}");

    // SERIALIZABLE rollback: status unchanged.
    let row = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
    assert_eq!(row.status, ACTIVE, "status flip must have rolled back");
}

// =====================================================================
// schedule_deletion
// =====================================================================

#[tokio::test]
async fn schedule_deletion_not_found() {
    let h = setup_sqlite().await.expect("setup");
    let err = h
        .repo
        .schedule_deletion(&allow_all(), Uuid::new_v4(), Uuid::nil(), now(), None)
        .await
        .expect_err("missing");
    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn schedule_deletion_idempotent_on_already_deleted() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    let deleted_at = now() - time::Duration::hours(5);
    insert_deleted_tenant(
        &h.provider,
        id,
        None,
        "tomb",
        0,
        deleted_at,
        Some(120),
        None,
        None,
        None,
    )
    .await
    .expect("seed tombstone");

    let out = h
        .repo
        .schedule_deletion(&allow_all(), id, Uuid::nil(), now(), None)
        .await
        .expect("idempotent");
    assert_eq!(out.status, TenantStatus::Deleted);

    // deleted_at MUST NOT be re-stamped (retention deadline preserved).
    let row = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
    assert_eq!(
        row.deleted_at
            .expect("tombstone deleted_at")
            .unix_timestamp(),
        deleted_at.unix_timestamp(),
        "idempotent re-delete must preserve the original deleted_at"
    );
}

#[tokio::test]
async fn schedule_deletion_rejects_provisioning() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    insert_provisioning_tenant(&h.provider, id, None, "prov", 0, now(), None, None, None)
        .await
        .expect("seed provisioning root");

    let err = h
        .repo
        .schedule_deletion(&allow_all(), id, Uuid::nil(), now(), None)
        .await
        .expect_err("provisioning cannot be soft-deleted");
    assert!(matches!(err, DomainError::Conflict { .. }), "got {err:?}");
}

#[tokio::test]
async fn schedule_deletion_rejects_when_live_children_present() {
    let h = setup_sqlite().await.expect("setup");
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    insert_tenant(&h.provider, parent, None, "parent", ACTIVE, false, 0)
        .await
        .expect("seed parent");
    insert_tenant(&h.provider, child, Some(parent), "child", ACTIVE, false, 1)
        .await
        .expect("seed child");

    let err = h
        .repo
        .schedule_deletion(&allow_all(), parent, Uuid::nil(), now(), None)
        .await
        .expect_err("parent with a live child cannot be soft-deleted");
    assert!(matches!(err, DomainError::TenantHasChildren), "got {err:?}");
}

#[tokio::test]
async fn schedule_deletion_stamps_deleted_at_and_retention_window() {
    let h = setup_sqlite().await.expect("setup");
    let id = Uuid::new_v4();
    insert_tenant(&h.provider, id, None, "live", ACTIVE, false, 0)
        .await
        .expect("seed");
    insert_closure(&h.provider, id, id, 0, ACTIVE)
        .await
        .expect("seed self-row");

    let out = h
        .repo
        .schedule_deletion(
            &allow_all(),
            id,
            Uuid::nil(),
            now(),
            Some(std::time::Duration::from_mins(15)),
        )
        .await
        .expect("soft-delete");
    assert_eq!(out.status, TenantStatus::Deleted);

    let row = fetch_tenant(&h.provider, id).await.unwrap().unwrap();
    assert_eq!(row.status, DELETED);
    assert!(row.deleted_at.is_some(), "deleted_at stamped");
    assert_eq!(
        row.retention_window_secs,
        Some(900),
        "per-call retention window stamped"
    );
}

// =====================================================================
// load_ancestor_chain_through_parent
// =====================================================================

#[tokio::test]
async fn ancestor_chain_resolves_via_closure() {
    let h = setup_sqlite().await.expect("setup");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    insert_tenant(&h.provider, root, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root");
    insert_tenant(&h.provider, child, Some(root), "child", ACTIVE, false, 1)
        .await
        .expect("seed child");
    insert_closure(&h.provider, child, child, 0, ACTIVE)
        .await
        .expect("child self");
    insert_closure(&h.provider, root, child, 0, ACTIVE)
        .await
        .expect("root->child");

    let chain = h
        .repo
        .load_ancestor_chain_through_parent(&allow_all(), child)
        .await
        .expect("chain");
    let ids: Vec<Uuid> = chain.iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![child, root], "leaf-first chain: parent then root");
}

#[tokio::test]
async fn ancestor_chain_empty_closure_falls_back_to_parent_walk() {
    let h = setup_sqlite().await.expect("setup");
    let root = Uuid::new_v4();
    // Root tenant with NO closure rows → fallback parent_id walk.
    insert_tenant(&h.provider, root, None, "lonely-root", ACTIVE, false, 0)
        .await
        .expect("seed");

    let chain = h
        .repo
        .load_ancestor_chain_through_parent(&allow_all(), root)
        .await
        .expect("fallback walk");
    let ids: Vec<Uuid> = chain.iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![root], "fallback walk yields the root alone");
}

#[tokio::test]
async fn ancestor_chain_fallback_missing_ancestor_is_not_found() {
    let h = setup_sqlite().await.expect("setup");
    // No tenant, no closure → fallback walk's first lookup misses.
    let err = h
        .repo
        .load_ancestor_chain_through_parent(&allow_all(), Uuid::new_v4())
        .await
        .expect_err("missing ancestor");
    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn ancestor_chain_non_contiguous_is_not_found() {
    let h = setup_sqlite().await.expect("setup");
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let wrong = Uuid::new_v4();
    insert_tenant(&h.provider, root, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root");
    // child claims `wrong` as parent_id, but closure links it to root.
    insert_tenant(&h.provider, child, Some(wrong), "child", ACTIVE, false, 1)
        .await
        .expect("seed child");
    insert_closure(&h.provider, child, child, 0, ACTIVE)
        .await
        .expect("child self");
    insert_closure(&h.provider, root, child, 0, ACTIVE)
        .await
        .expect("root->child");

    let err = h
        .repo
        .load_ancestor_chain_through_parent(&allow_all(), child)
        .await
        .expect_err("non-contiguous chain");
    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn ancestor_chain_count_mismatch_is_not_found() {
    let h = setup_sqlite().await.expect("setup");
    let child = Uuid::new_v4();
    let ghost = Uuid::new_v4(); // referenced by closure but has no tenant row
    insert_tenant(&h.provider, child, Some(ghost), "child", ACTIVE, false, 1)
        .await
        .expect("seed child");
    insert_closure(&h.provider, child, child, 0, ACTIVE)
        .await
        .expect("child self");
    insert_closure(&h.provider, ghost, child, 0, ACTIVE)
        .await
        .expect("ghost->child");

    let err = h
        .repo
        .load_ancestor_chain_through_parent(&allow_all(), child)
        .await
        .expect_err("closure names a non-existent ancestor");
    assert!(matches!(err, DomainError::NotFound { .. }), "got {err:?}");
}
