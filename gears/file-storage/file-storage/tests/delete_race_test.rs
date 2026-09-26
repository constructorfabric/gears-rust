//! Deterministic regression tests for the file/version delete race: the version list (and the
//! "is this the file's only version?" decision) used to be read **before**
//! the delete's own transaction ever opened, via a plain, separately-called
//! `Store::list_versions`. A version inserted by a concurrent
//! `presign_version`/`initiate_multipart_upload` on the same `file_id`, any
//! time after that early read and before the delete transaction's commit,
//! was invisible to it:
//!
//! - `delete_file_inner`/retention-expiry's `expire_file` would cascade-remove
//!   that version's row without ever queuing its backend blob for cleanup --
//!   a permanent storage leak.
//! - `delete_version`'s "only one version left -- delete the whole file"
//!   branch would fire on a stale count and delete a **different**,
//!   concurrently-added version along with the file, even though the caller
//!   only ever asked to remove one specific version.
//!
//! The fix (`Store::delete_file_collecting_versions`,
//! `Store::delete_version_or_whole_file`) re-reads the version list as the
//! FIRST statement of the transaction that performs the delete, immediately
//! before it, instead of trusting an old snapshot. Both tests below construct
//! the race deterministically at the `Store` level (no wall-clock
//! sleep/thread-timing dependence, mirroring `tests/version_repo_test.rs`'s
//! and `tests/store_files_test.rs`'s own direct-`Store`-call convention): a
//! version is inserted-and-committed for the target file, and only THEN is
//! the fixed delete method invoked -- exactly the ordering that a
//! concurrent writer winning the race against an old, early, pre-transaction
//! snapshot would produce. A bare `Store::list_versions` call captured at the
//! point the old (now-removed) code used to read from (each test's very
//! first statement, before the race version exists) is kept alongside as the
//! explicit counterfactual: it does NOT see the race version, which is
//! exactly the stale input the pre-fix code would have handed to the delete.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation, AuditOutcome, FileEvent};
use file_storage::domain::ports::DeleteVersionOutcome;
use file_storage::infra::backend::{InMemoryBackend, StorageBackend};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::NewFile;

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// Write the whole of `bytes` to `path` via `put_stream` (a one-shot
/// stream) -- the test-only stand-in for the whole-object `put` the trait no
/// longer has.
async fn write_all(backend: &Arc<dyn StorageBackend>, path: &str, bytes: bytes::Bytes) {
    let len = bytes.len() as u64;
    let stream: futures::stream::BoxStream<'static, std::io::Result<bytes::Bytes>> =
        Box::pin(futures::stream::once(async move { Ok(bytes) }));
    backend
        .put_stream(path, stream, Some(len))
        .await
        .expect("put_stream");
}

/// A unique temp-file SQLite DB, migrated (mirrors `tests/store_files_test.rs`).
async fn build_store() -> (Store, Arc<DBProvider<DbError>>) {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-delete-race-{}.db", Uuid::now_v7().simple()));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let opts = ConnectOpts {
        max_conns: Some(2),
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

fn new_file_req(owner_id: Uuid) -> NewFile {
    NewFile {
        owner_kind: file_storage_sdk::OwnerKind::User,
        owner_id,
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: Vec::new(),
    }
}

fn audit(
    tenant_id: Uuid,
    file_id: Uuid,
    op: AuditOperation,
    detail: serde_json::Value,
) -> AuditEntry {
    AuditEntry {
        tenant_id,
        actor_kind: "user".to_owned(),
        actor_id: Uuid::now_v7(),
        file_id: Some(file_id),
        operation: op,
        outcome: AuditOutcome::Success,
        detail,
        occurred_at: OffsetDateTime::now_utc(),
    }
}

fn event(tenant_id: Uuid, owner_id: Uuid, file_id: Uuid) -> FileEvent {
    FileEvent {
        tenant_id,
        owner_id,
        file_id,
        event_type: "file.deleted".to_owned(),
        payload: serde_json::json!({ "version_count": 0 }),
    }
}

// ── delete_file vs a concurrent new version ─────────────────────────────────

/// A version inserted for `file_id` after the (stale, pre-fix-style) version
/// snapshot was taken, but before the whole-file delete runs, must still be
/// collected by [`Store::delete_file_collecting_versions`] and have its
/// backend blob actually removed -- not silently cascade-deleted from the DB
/// while its blob is orphaned forever.
#[tokio::test]
async fn delete_file_collecting_versions_does_not_leak_a_concurrently_added_version_blob() {
    let (store, _db) = build_store().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));

    let tenant_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();

    store
        .create_file_with_pending_version(
            &new_file_req(owner_id),
            file_id,
            v1,
            tenant_id,
            "mem",
            &format!("/{file_id}/{v1}"),
            now,
            audit(
                tenant_id,
                file_id,
                AuditOperation::Create,
                serde_json::json!({}),
            ),
        )
        .await
        .expect("create file + v1");
    write_all(
        &backend,
        &format!("/{file_id}/{v1}"),
        bytes::Bytes::from_static(b"v1"),
    )
    .await;

    // The old code's pre-transaction snapshot, captured at this exact point
    // -- before the race version exists -- is the stale input the pre-fix
    // `delete_file_inner` would have handed to the (unconditional) delete.
    let stale_snapshot = store.list_versions(file_id).await.expect("stale snapshot");
    assert_eq!(
        stale_snapshot.len(),
        1,
        "the stale snapshot must not see the version inserted below -- this \
         is exactly what made the pre-fix code lose track of it"
    );

    // The race: a concurrent `presign_version` commits a second version for
    // the same file, after the stale snapshot above, before the delete runs.
    let v2 = Uuid::now_v7();
    store
        .insert_pending_version(
            file_id,
            v2,
            "application/octet-stream",
            "mem",
            &format!("/{file_id}/{v2}"),
            now,
        )
        .await
        .expect("insert v2 (the race)");
    write_all(
        &backend,
        &format!("/{file_id}/{v2}"),
        bytes::Bytes::from_static(b"v2"),
    )
    .await;

    let deleted = store
        .delete_file_collecting_versions(
            &toolkit_security::AccessScope::allow_all(),
            file_id,
            audit(
                tenant_id,
                file_id,
                AuditOperation::DeleteFile,
                serde_json::json!({ "version_count": 0 }),
            ),
            Some(event(tenant_id, owner_id, file_id)),
        )
        .await
        .expect("delete_file_collecting_versions must not error");

    assert!(deleted.removed, "the file row must be found and removed");
    let mut collected_ids: Vec<Uuid> = deleted.versions.iter().map(|v| v.version_id).collect();
    collected_ids.sort_unstable();
    let mut expected_ids = vec![v1, v2];
    expected_ids.sort_unstable();
    assert_eq!(
        collected_ids, expected_ids,
        "the collected version list must include the race version (v2), not \
         just the versions visible at the stale pre-transaction snapshot"
    );

    // Best-effort blob cleanup, exactly as `FileService::delete_file_inner`
    // performs it from this same returned list.
    for v in &deleted.versions {
        backend
            .delete(&v.backend_path)
            .await
            .expect("best-effort delete");
    }
    for v in &deleted.versions {
        assert!(
            !backend.exists(&v.backend_path).await.unwrap(),
            "blob for {} must be gone -- no leaked backend storage",
            v.version_id
        );
    }
}

// ── delete_version vs a concurrent new version ──────────────────────────────

/// `delete_version_or_whole_file`'s "is `version_id` the file's only
/// version?" decision must be made fresh, inside its own transaction: a
/// second version inserted after the stale (pre-fix-style) count was taken
/// must NOT cause the whole file to be deleted -- only the requested
/// version.
#[tokio::test]
async fn delete_version_or_whole_file_does_not_delete_the_file_when_a_second_version_races_in() {
    let (store, _db) = build_store().await;

    let tenant_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();

    store
        .create_file_with_pending_version(
            &new_file_req(owner_id),
            file_id,
            v1,
            tenant_id,
            "mem",
            &format!("/{file_id}/{v1}"),
            now,
            audit(
                tenant_id,
                file_id,
                AuditOperation::Create,
                serde_json::json!({}),
            ),
        )
        .await
        .expect("create file + v1");

    // The old code's stale pre-transaction count: exactly 1, which used to
    // make `delete_version` take its "last version -> delete the whole
    // file" branch.
    let stale_snapshot = store.list_versions(file_id).await.expect("stale snapshot");
    assert_eq!(stale_snapshot.len(), 1, "only v1 exists at this point");

    // The race: a second version is inserted (and committed) for the same
    // file after that stale count, before `DELETE /files/{id}/versions/{v1}`
    // actually runs.
    let v2 = Uuid::now_v7();
    store
        .insert_pending_version(
            file_id,
            v2,
            "application/octet-stream",
            "mem",
            &format!("/{file_id}/{v2}"),
            now,
        )
        .await
        .expect("insert v2 (the race)");

    let scope = toolkit_security::AccessScope::allow_all();
    let outcome = store
        .delete_version_or_whole_file(
            file_id,
            v1,
            audit(
                tenant_id,
                file_id,
                AuditOperation::DeleteVersion,
                serde_json::json!({ "version_id": v1 }),
            ),
            audit(
                tenant_id,
                file_id,
                AuditOperation::DeleteFile,
                serde_json::json!({ "version_count": 1 }),
            ),
            Some(event(tenant_id, owner_id, file_id)),
        )
        .await
        .expect("delete_version_or_whole_file must not error");

    assert!(
        matches!(outcome, DeleteVersionOutcome::VersionRemoved(ref removed) if removed.version_id == v1),
        "expected VersionRemoved(v1) since a second version exists by the \
         time the transaction actually runs -- got {outcome:?}"
    );

    let file = store
        .get_file(&scope, file_id)
        .await
        .expect("get_file must not error");
    assert!(
        file.is_some(),
        "the file must survive -- v1 was NOT its only version by delete time"
    );
    let remaining_v2 = store
        .get_version(file_id, v2)
        .await
        .expect("get_version must not error");
    assert!(
        remaining_v2.is_some(),
        "v2 (the race version, never asked to be deleted) must still exist"
    );
    let removed_v1 = store
        .get_version(file_id, v1)
        .await
        .expect("get_version must not error");
    assert!(
        removed_v1.is_none(),
        "v1 (the requested version) must be gone"
    );
}
