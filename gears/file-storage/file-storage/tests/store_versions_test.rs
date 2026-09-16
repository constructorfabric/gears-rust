//! `Store`-level tests for `src/infra/storage/store/versions.rs`.
//!
//! Covers the CAS + promote sequence shared by `bind_atomic`,
//! `bind_atomic_with_event`, and `finalize_version`'s auto-bind branch: a
//! promotion (`VersionRepo::set_current`) that affects zero rows must abort
//! the whole transaction with `DomainError::conflict`, never commit a
//! `files.content_id` that points at a version row which does not exist
//! (there is no FK from `files.content_id` to `file_versions` in this
//! schema, so nothing but this in-transaction guard prevents that dangling
//! pointer). Also covers the successful bind path and `finalize_version`'s
//! `auto_bind` / no-`auto_bind` / lost-CAS outcomes.
//!
//! Uses a temp-file SQLite DB (mirrors `tests/version_repo_test.rs` /
//! `tests/policy_test.rs`): a bare `sqlite::memory:` would give each pooled
//! connection its own empty DB. State is asserted with direct `FileRepo`/
//! `VersionRepo` reads (`AccessScope::allow_all()`), never through a scoped
//! service read, per this repo's direct-DB-assertion convention.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use file_storage::domain::error::DomainError;
use file_storage::domain::ports::AutoBindOnFinalize;
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::{FileRepo, VersionRepo};
use file_storage_sdk::{File, FileVersion, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file SQLite DB, migrated. Mirrors `tests/policy_test.rs`'s
/// `build_store` -- the caller keeps the `Arc<DBProvider<..>>` alongside the
/// `Store` so it can open its own connection for direct repo-level reads.
async fn build_store() -> (Store, Arc<DBProvider<DbError>>) {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-store-versions-{}.db",
        Uuid::now_v7().simple()
    ));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let conn = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&conn, Migrator::migrations())
        .await
        .expect("migrations");
    let db: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(conn));
    (Store::new(Arc::clone(&db)), db)
}

fn new_file(file_id: Uuid, tenant_id: Uuid, content_id: Option<Uuid>) -> File {
    let now = OffsetDateTime::now_utc();
    File {
        file_id,
        tenant_id,
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        content_id,
        meta_version: 0,
        created_at: now,
        last_modified_at: now,
    }
}

fn new_version(
    file_id: Uuid,
    version_id: Uuid,
    status: VersionStatus,
    is_current: bool,
) -> FileVersion {
    let now = OffsetDateTime::now_utc();
    FileVersion {
        file_id,
        version_id,
        mime_type: "application/octet-stream".to_owned(),
        size: 0,
        hash_algorithm: "SHA-256".to_owned(),
        hash_value: vec![0u8; 32],
        hash_mode: HashMode::WholeSha256.as_str().to_owned(),
        part_count: None,
        status,
        is_current,
        backend_id: "mem".to_owned(),
        backend_path: format!("/{file_id}/{version_id}"),
        created_at: now,
    }
}

fn audit_entry(tenant_id: Uuid, file_id: Uuid, op: AuditOperation) -> AuditEntry {
    AuditEntry::success(
        tenant_id,
        "user",
        Uuid::now_v7(),
        Some(file_id),
        op,
        serde_json::json!({}),
    )
}

fn file_event(tenant_id: Uuid, owner_id: Uuid, file_id: Uuid, event_type: &str) -> FileEvent {
    FileEvent {
        tenant_id,
        owner_id,
        file_id,
        event_type: event_type.to_owned(),
        payload: serde_json::json!({}),
    }
}

// -- bind_atomic --------------------------------------------------------------

/// A `bind_atomic` call whose `version_id` was never inserted must fail with
/// `Conflict` (the `set_current` guard in versions.rs), and must leave
/// `files.content_id` and the previously-current version's `is_current` flag
/// completely untouched -- the earlier, successful half of the same
/// transaction (the CAS swap and `clear_current`) must roll back too.
#[tokio::test]
async fn bind_atomic_conflict_when_target_version_missing_rolls_back_content_id() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
    let missing = Uuid::now_v7(); // never inserted into file_versions

    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, v1, VersionStatus::Available, false),
        )
        .await
        .expect("insert v1");

    let bound = store
        .bind_atomic(
            &scope,
            file_id,
            None,
            v1,
            OffsetDateTime::now_utc(),
            audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
        )
        .await
        .expect("binding v1 onto a fresh file must succeed");
    assert!(bound, "first bind must swap content_id from NULL to v1");

    let err = store
        .bind_atomic(
            &scope,
            file_id,
            Some(v1),
            missing,
            OffsetDateTime::now_utc(),
            audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
        )
        .await
        .expect_err("binding a version that does not exist must fail");
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict, got {err:?}"
    );

    let file = files
        .get(&conn, &scope, file_id)
        .await
        .expect("get file")
        .expect("file exists");
    assert_eq!(
        file.content_id,
        Some(v1),
        "content_id must not move onto a version that does not exist"
    );
    let v1_row = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .expect("get v1")
        .expect("v1 exists");
    assert!(
        v1_row.is_current,
        "clear_current's effect must have rolled back along with the failed promote"
    );

    let audit_rows = store.list_audit(file_id).await.expect("list_audit");
    assert_eq!(
        audit_rows.len(),
        1,
        "only the first, successful bind may have written an audit row"
    );
}

/// The successful bind path: `content_id` is swapped, exactly one version of
/// the file ends up `is_current`, and one audit row is written per call.
#[tokio::test]
async fn bind_atomic_success_swaps_content_and_sets_single_current() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
    let v2 = Uuid::now_v7();

    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, v1, VersionStatus::Available, false),
        )
        .await
        .expect("insert v1");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, v2, VersionStatus::Available, false),
        )
        .await
        .expect("insert v2");

    let ok = store
        .bind_atomic(
            &scope,
            file_id,
            None,
            v1,
            OffsetDateTime::now_utc(),
            audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
        )
        .await
        .expect("first bind must not error");
    assert!(ok);

    let file = files.get(&conn, &scope, file_id).await.unwrap().unwrap();
    assert_eq!(file.content_id, Some(v1));
    let v1_row = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .unwrap()
        .unwrap();
    assert!(v1_row.is_current, "v1 must become current after its bind");

    let ok2 = store
        .bind_atomic(
            &scope,
            file_id,
            Some(v1),
            v2,
            OffsetDateTime::now_utc(),
            audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
        )
        .await
        .expect("second bind must not error");
    assert!(ok2);

    let refetched = files.get(&conn, &scope, file_id).await.unwrap().unwrap();
    assert_eq!(refetched.content_id, Some(v2), "content_id must move to v2");
    let v1_after = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .unwrap()
        .unwrap();
    let v2_after = versions
        .get(&conn, &scope, file_id, v2)
        .await
        .unwrap()
        .unwrap();
    assert!(!v1_after.is_current, "old current must be cleared");
    assert!(v2_after.is_current, "new version must be marked current");

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(
        audit_rows.len(),
        2,
        "each successful bind must write exactly one audit row"
    );
}

// -- bind_atomic_with_event -----------------------------------------------------

/// The events-aware variant must roll back exactly like `bind_atomic`: a
/// conflict must leave `content_id` untouched and enqueue no event, even
/// though the caller passed one.
#[tokio::test]
async fn bind_atomic_with_event_conflict_when_target_version_missing_enqueues_no_event() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
    let missing = Uuid::now_v7();

    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, v1, VersionStatus::Available, false),
        )
        .await
        .expect("insert v1");

    store
        .bind_atomic_with_event(
            &scope,
            file_id,
            None,
            v1,
            OffsetDateTime::now_utc(),
            audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
            None,
        )
        .await
        .expect("first bind must succeed");

    let err = store
        .bind_atomic_with_event(
            &scope,
            file_id,
            Some(v1),
            missing,
            OffsetDateTime::now_utc(),
            audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
            Some(file_event(
                tenant_id,
                owner_id,
                file_id,
                "file.content_updated",
            )),
        )
        .await
        .expect_err("binding a missing version must fail");
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict, got {err:?}"
    );

    let file = files.get(&conn, &scope, file_id).await.unwrap().unwrap();
    assert_eq!(file.content_id, Some(v1), "content_id must not change");

    let events = store.list_file_events(file_id).await.unwrap();
    assert!(
        events.is_empty(),
        "no event may be enqueued when the enclosing transaction rolls back"
    );
}

/// The events-aware variant's successful path enqueues the given event in
/// the same transaction as the bind.
#[tokio::test]
async fn bind_atomic_with_event_success_enqueues_event() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();

    FileRepo::new()
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, v1, VersionStatus::Available, false),
        )
        .await
        .expect("insert v1");

    let ok = store
        .bind_atomic_with_event(
            &scope,
            file_id,
            None,
            v1,
            OffsetDateTime::now_utc(),
            audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
            Some(file_event(
                tenant_id,
                owner_id,
                file_id,
                "file.content_updated",
            )),
        )
        .await
        .expect("bind must succeed");
    assert!(ok);

    let events = store.list_file_events(file_id).await.unwrap();
    assert_eq!(
        events.len(),
        1,
        "the bind's event must be enqueued exactly once"
    );
    assert_eq!(events[0].event_type, "file.content_updated");
    assert_eq!(events[0].file_id, file_id);

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(audit_rows.len(), 1);
}

// -- finalize_version -----------------------------------------------------------

/// `finalize_version` without `auto_bind` marks the version `available` and
/// leaves `files.content_id` completely untouched, even when the file
/// already has unrelated content bound.
#[tokio::test]
async fn finalize_version_without_auto_bind_marks_available_leaves_content_id_unchanged() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let bound_elsewhere = Uuid::now_v7(); // stands in for an unrelated current version
    let v_new = Uuid::now_v7();

    files
        .create(
            &conn,
            &scope,
            &new_file(file_id, tenant_id, Some(bound_elsewhere)),
        )
        .await
        .expect("create file");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, v_new, VersionStatus::Pending, false),
        )
        .await
        .expect("insert pending version");

    let outcome = store
        .finalize_version(
            file_id,
            v_new,
            123,
            vec![7u8; 32],
            HashMode::WholeSha256,
            None,
            None,
            Some("text/plain".to_owned()),
            audit_entry(tenant_id, file_id, AuditOperation::FinalizeVersion),
            None,
        )
        .await
        .expect("finalize must not error");
    assert!(outcome.updated, "the pending row must be finalized");
    assert!(!outcome.bound, "no auto_bind was requested");

    let v_row = versions
        .get(&conn, &scope, file_id, v_new)
        .await
        .unwrap()
        .expect("version exists");
    assert_eq!(v_row.status, VersionStatus::Available);
    assert_eq!(v_row.size, 123);
    assert_eq!(v_row.mime_type, "text/plain");
    assert!(
        !v_row.is_current,
        "no bind was requested, so it stays uncurrent"
    );

    let file = files.get(&conn, &scope, file_id).await.unwrap().unwrap();
    assert_eq!(
        file.content_id,
        Some(bound_elsewhere),
        "content_id must be untouched by a finalize with no auto_bind"
    );

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(audit_rows.len(), 1, "only the finalize audit row");
}

/// `finalize_version` with `auto_bind`: the version becomes `available` AND
/// is bound as the file's current content, in the same transaction, with
/// both audit rows and the bind's event all committed together.
#[tokio::test]
async fn finalize_version_with_auto_bind_marks_available_and_binds_content() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();

    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, v1, VersionStatus::Pending, false),
        )
        .await
        .expect("insert pending version");

    let auto_bind = AutoBindOnFinalize {
        expected_content_id: None,
        audit: audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
        event: Some(file_event(
            tenant_id,
            owner_id,
            file_id,
            "file.content_updated",
        )),
    };

    let outcome = store
        .finalize_version(
            file_id,
            v1,
            456,
            vec![9u8; 32],
            HashMode::WholeSha256,
            None,
            None,
            None,
            audit_entry(tenant_id, file_id, AuditOperation::FinalizeVersion),
            Some(auto_bind),
        )
        .await
        .expect("finalize with auto_bind must not error");
    assert!(outcome.updated);
    assert!(
        outcome.bound,
        "the CAS must win on a first bind (expected None)"
    );

    let v_row = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .unwrap()
        .expect("version exists");
    assert_eq!(v_row.status, VersionStatus::Available);
    assert!(v_row.is_current, "auto_bind must mark the version current");

    let file = files.get(&conn, &scope, file_id).await.unwrap().unwrap();
    assert_eq!(file.content_id, Some(v1), "auto_bind must set content_id");

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(audit_rows.len(), 2, "finalize row + bind row");
    let events = store.list_file_events(file_id).await.unwrap();
    assert_eq!(
        events.len(),
        1,
        "the auto_bind's event must be enqueued once"
    );
}

/// A lost finalize CAS (the version is no longer `pending`, e.g. a racing
/// finalize already won) must report `updated: false` / `bound: false`
/// without an error, must not touch the version row at all (its size/hash/
/// mime_type from the earlier, real finalize are preserved), and must not
/// write a second audit row.
#[tokio::test]
async fn finalize_version_lost_cas_when_not_pending_leaves_row_untouched() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();

    FileRepo::new()
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, v1, VersionStatus::Pending, false),
        )
        .await
        .expect("insert pending version");

    // Finalize it once, directly through the repo, bypassing `Store` (no
    // audit row) -- this is the "already available" state a second,
    // racing finalize call would observe.
    let first_hash = vec![1u8; 32];
    let first_updated = versions
        .finalize(
            &conn,
            &scope,
            file_id,
            v1,
            42,
            first_hash.clone(),
            "whole-sha256",
            None,
            Some("text/plain".to_owned()),
        )
        .await
        .expect("direct finalize must succeed");
    assert!(first_updated, "the version was pending, so this must win");

    // A second finalize call through `Store` for the same version, now that
    // it is `available` -- the CAS predicate (`status = pending`) matches
    // zero rows.
    let outcome = store
        .finalize_version(
            file_id,
            v1,
            999,
            vec![2u8; 32],
            HashMode::WholeSha256,
            None,
            None,
            Some("application/json".to_owned()),
            audit_entry(tenant_id, file_id, AuditOperation::FinalizeVersion),
            None,
        )
        .await
        .expect("a lost CAS is reported, not an error");
    assert!(!outcome.updated, "the version is no longer pending");
    assert!(!outcome.bound, "no bind can happen when updated is false");

    let v_row = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .unwrap()
        .expect("version still exists");
    assert_eq!(v_row.status, VersionStatus::Available);
    assert_eq!(v_row.size, 42, "the lost CAS must not overwrite size");
    assert_eq!(
        v_row.hash_value, first_hash,
        "the lost CAS must not overwrite hash_value"
    );
    assert_eq!(
        v_row.mime_type, "text/plain",
        "the lost CAS must not overwrite mime_type"
    );

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert!(
        audit_rows.is_empty(),
        "a lost CAS (updated == false) must never write an audit row"
    );
}
