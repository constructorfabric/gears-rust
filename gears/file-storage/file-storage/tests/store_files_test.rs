//! `Store`-level tests for `src/infra/storage/store/files.rs`.
//!
//! Covers: the conditional-`DELETE` orphan guard in
//! `delete_orphan_file_with_event` (`content_id IS NULL AND NOT EXISTS (...
//! file_versions ...)` as one statement, per its own doc comment and
//! `FileRepo::delete_if_orphan`'s), duplicate-key handling of a file's
//! initial `custom_metadata` batch, and cascade-delete via
//! `delete_file_with_event`.
//!
//! Uses a temp-file SQLite DB (mirrors `tests/version_repo_test.rs` /
//! `tests/policy_test.rs`): a bare `sqlite::memory:` would give each pooled
//! connection its own empty DB. State is asserted with direct `FileRepo`/
//! `VersionRepo`/`MetadataRepo` reads (`AccessScope::allow_all()`), never
//! through a scoped service read, per this repo's direct-DB-assertion
//! convention.

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
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::{FileRepo, MetadataRepo, VersionRepo};
use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, NewFile, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file SQLite DB, migrated. Mirrors `tests/policy_test.rs`'s
/// `build_store` -- the caller keeps the `Arc<DBProvider<..>>` alongside the
/// `Store` so it can open its own connection for direct repo-level reads.
async fn build_store() -> (Store, Arc<DBProvider<DbError>>) {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-store-files-{}.db", Uuid::now_v7().simple()));
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

fn new_file_req(owner_id: Uuid, custom_metadata: Vec<CustomMetadataEntry>) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id,
        name: "created.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata,
    }
}

// -- delete_orphan_file_with_event ----------------------------------------------

/// A file with `content_id IS NULL` and zero `file_versions` rows is a true
/// orphan: it must be removed, with its audit row and event both written.
#[tokio::test]
async fn files_delete_orphan_removes_file_with_no_content_and_no_versions() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();

    let tenant_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create orphan file");

    let removed = store
        .delete_orphan_file_with_event(
            file_id,
            audit_entry(tenant_id, file_id, AuditOperation::OrphanReconcile),
            Some(file_event(tenant_id, owner_id, file_id, "file.deleted")),
        )
        .await
        .expect("delete_orphan_file_with_event must not error");
    assert!(
        removed,
        "a file with no content and no versions is a true orphan"
    );

    assert!(
        files.get(&conn, &scope, file_id).await.unwrap().is_none(),
        "the file row must be removed"
    );
    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(
        audit_rows.len(),
        1,
        "the orphan-reconcile audit row must be written"
    );
    let events = store.list_file_events(file_id).await.unwrap();
    assert_eq!(events.len(), 1, "the deletion event must be enqueued");
}

/// A file with bound content (`content_id` set) must never be treated as an
/// orphan, regardless of its version count -- the guard's `content_id IS
/// NULL` half must reject the delete.
#[tokio::test]
async fn files_delete_orphan_keeps_file_with_bound_content() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();

    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let fake_content = Uuid::now_v7();
    files
        .create(
            &conn,
            &scope,
            &new_file(file_id, tenant_id, Some(fake_content)),
        )
        .await
        .expect("create file with bound content");

    let removed = store
        .delete_orphan_file_with_event(
            file_id,
            audit_entry(tenant_id, file_id, AuditOperation::OrphanReconcile),
            None,
        )
        .await
        .expect("delete_orphan_file_with_event must not error");
    assert!(
        !removed,
        "a file with bound content must never be reclaimed as orphan"
    );

    assert!(
        files.get(&conn, &scope, file_id).await.unwrap().is_some(),
        "the file row must survive"
    );
    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert!(
        audit_rows.is_empty(),
        "no audit row when the guard rejects the delete"
    );
}

/// A file with at least one `file_versions` row (even with `content_id`
/// still NULL, e.g. a pending upload) must never be treated as an orphan --
/// the guard's `NOT EXISTS` half must reject the delete.
#[tokio::test]
async fn files_delete_orphan_keeps_file_with_a_version_row() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, version_id, VersionStatus::Pending, false),
        )
        .await
        .expect("insert pending version");

    let removed = store
        .delete_orphan_file_with_event(
            file_id,
            audit_entry(tenant_id, file_id, AuditOperation::OrphanReconcile),
            None,
        )
        .await
        .expect("delete_orphan_file_with_event must not error");
    assert!(
        !removed,
        "a file with a version row must never be reclaimed as orphan"
    );

    assert!(
        files.get(&conn, &scope, file_id).await.unwrap().is_some(),
        "the file row must survive"
    );
    assert!(
        versions
            .get(&conn, &scope, file_id, version_id)
            .await
            .unwrap()
            .is_some(),
        "the version row must survive untouched"
    );
    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert!(
        audit_rows.is_empty(),
        "no audit row when the guard rejects the delete"
    );
}

// -- create_file_with_pending_version: initial metadata batch --------------------

/// Creating a file with duplicate keys in its initial `custom_metadata` must
/// not fail the whole transaction: the batch insert dedups first, and the
/// last occurrence of a repeated key wins.
#[tokio::test]
async fn files_create_dedups_duplicate_initial_metadata_keys_last_wins() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let metadata = MetadataRepo::new();

    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();

    let new = new_file_req(
        owner_id,
        vec![
            CustomMetadataEntry {
                key: "k1".to_owned(),
                value: "first".to_owned(),
            },
            CustomMetadataEntry {
                key: "k2".to_owned(),
                value: "only".to_owned(),
            },
            CustomMetadataEntry {
                key: "k1".to_owned(),
                value: "second".to_owned(),
            },
        ],
    );

    store
        .create_file_with_pending_version(
            &new,
            file_id,
            version_id,
            tenant_id,
            "mem",
            "/mem/path",
            now,
            audit_entry(tenant_id, file_id, AuditOperation::Create),
        )
        .await
        .expect("create must succeed despite a duplicate metadata key");

    let mut entries = metadata.list(&conn, &scope, file_id).await.unwrap();
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(entries.len(), 2, "duplicate key must collapse into one row");
    assert_eq!(entries[0].key, "k1");
    assert_eq!(
        entries[0].value, "second",
        "last occurrence of a repeated key wins"
    );
    assert_eq!(entries[1].key, "k2");
    assert_eq!(entries[1].value, "only");
}

// -- delete_file_with_event ------------------------------------------------------

/// Deleting a file removes the `files` row, cascades its version and
/// metadata rows, and writes an audit row plus the given event -- all
/// checked with direct entity reads.
#[tokio::test]
async fn files_delete_with_event_cascades_versions_and_metadata() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let metadata = MetadataRepo::new();

    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();

    let new = new_file_req(
        owner_id,
        vec![CustomMetadataEntry {
            key: "k1".to_owned(),
            value: "v1".to_owned(),
        }],
    );
    store
        .create_file_with_pending_version(
            &new,
            file_id,
            version_id,
            tenant_id,
            "mem",
            "/mem/path",
            now,
            audit_entry(tenant_id, file_id, AuditOperation::Create),
        )
        .await
        .expect("create must succeed");

    // Sanity: the rows this test proves get cascade-removed actually exist
    // before the delete.
    assert!(files.get(&conn, &scope, file_id).await.unwrap().is_some());
    assert!(
        versions
            .get(&conn, &scope, file_id, version_id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        metadata.list(&conn, &scope, file_id).await.unwrap().len(),
        1
    );

    let removed = store
        .delete_file_with_event(
            &scope,
            file_id,
            audit_entry(tenant_id, file_id, AuditOperation::DeleteFile),
            Some(file_event(tenant_id, owner_id, file_id, "file.deleted")),
        )
        .await
        .expect("delete_file_with_event must not error");
    assert!(removed, "the file row must be found and removed");

    assert!(
        files.get(&conn, &scope, file_id).await.unwrap().is_none(),
        "the file row must be gone"
    );
    assert!(
        versions
            .get(&conn, &scope, file_id, version_id)
            .await
            .unwrap()
            .is_none(),
        "the version row must cascade-delete with its parent file"
    );
    assert!(
        metadata
            .list(&conn, &scope, file_id)
            .await
            .unwrap()
            .is_empty(),
        "metadata rows must cascade-delete with their parent file"
    );

    // The outbox tables carry no FK to `files`, so both the create and the
    // delete audit rows / delete event must still be readable afterward.
    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(audit_rows.len(), 2, "create audit row + delete audit row");
    let events = store.list_file_events(file_id).await.unwrap();
    assert!(
        events.iter().any(|e| e.event_type == "file.deleted"),
        "expected a file.deleted event"
    );
}
