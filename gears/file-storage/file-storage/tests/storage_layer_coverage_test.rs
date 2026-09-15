//! Coverage top-up for the storage layer (repo + `Store` levels), targeting
//! specific methods left unexercised by the existing test suite:
//!
//! - `MultipartRepo::abort_expired_completing`'s lease-fencing CAS.
//! - `VersionRepo::get_manifests`'s batched lookup (plus a direct check of
//!   `set_current`/`clear_current`'s two return-value meanings -- see
//!   `VersionRepo::set_current`'s doc comment: a `0` result from
//!   `set_current` must abort the caller's transaction, while a `0` from
//!   `clear_current` is the ordinary "nothing to clear" case).
//! - `Store::{upsert_multipart_part, abort_multipart_upload,
//!   delete_parts_for_upload}` in `store/multipart.rs`.
//! - `Store::{get_version_manifests, bind_atomic, bind_atomic_with_event}`'s
//!   CAS-mismatch / wrapper paths in `store/versions.rs`.
//! - `Store::{delete_file, create_file_with_event}` in `store/files.rs`.
//! - The `CleanupStore`/`MultipartStore` trait-forwarding methods in
//!   `store/traits.rs`.
//!
//! Mirrors `tests/version_repo_test.rs` / `tests/store_versions_test.rs` /
//! `tests/store_files_test.rs` / `tests/multipart_repo_test.rs`: a temp-file
//! SQLite DB with the full migration applied (a bare `sqlite::memory:` would
//! give each pooled connection its own empty DB), `AccessScope::allow_all()`
//! throughout, and state asserted with direct repo reads rather than through
//! a scoped service read.

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
use file_storage::domain::multipart::MultipartUploadState;
use file_storage::domain::ports::{CleanupStore, MultipartStore};
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::{FileRepo, MetadataRepo, MultipartRepo, VersionRepo};
use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, NewFile, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file SQLite DB, migrated -- mirrors every sibling
/// `*_test.rs`'s `db()`/`build_store()`.
async fn build_store() -> (Store, Arc<DBProvider<DbError>>) {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-storage-layer-coverage-{}.db",
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

/// Seed one `files` row and one `multipart_uploads` session
/// (`state = in_progress`) for it, returning `(file_id, upload_id)`.
/// Mirrors `tests/multipart_repo_test.rs::seed_session`.
async fn seed_session<C: toolkit_db::secure::DBRunner>(
    conn: &C,
    multipart: &MultipartRepo,
    expires_at: OffsetDateTime,
    now: OffsetDateTime,
) -> (Uuid, Uuid) {
    let files = FileRepo::new();
    let scope = AccessScope::allow_all();
    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    files
        .create(conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create parent file");

    let upload_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    multipart
        .create(
            conn,
            upload_id,
            file_id,
            version_id,
            "backend-handle",
            "application/octet-stream",
            100,
            50,
            false,
            expires_at,
            now,
        )
        .await
        .expect("create multipart session");
    (file_id, upload_id)
}

// ===========================================================================
// multipart_repo.rs -- MultipartRepo::abort_expired_completing
// ===========================================================================

/// A `completing` session whose lease has already expired (`lease_until <
/// now`) is aborted: the CAS succeeds, the row visibly moves to `aborted`
/// with both lease fields cleared.
#[tokio::test]
async fn abort_expired_completing_succeeds_when_lease_has_expired() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);
    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

    // Move the session into `completing` with a `lease_until` already in
    // the past relative to `now` -- the lease's own value, not the CAS's
    // `expires_at` fence, is what `abort_expired_completing` checks.
    let past_lease = now - time::Duration::minutes(10);
    let acquired = multipart
        .acquire_complete_lease(&conn, upload_id, "completer-a", past_lease, now)
        .await
        .expect("acquire_complete_lease must not error");
    assert!(
        acquired,
        "setup: a fresh in_progress session must accept the lease"
    );

    let aborted = multipart
        .abort_expired_completing(&conn, upload_id, now)
        .await
        .expect("abort_expired_completing must not error");
    assert!(aborted, "an expired lease must be abortable");

    let session = multipart
        .get(&conn, upload_id)
        .await
        .expect("get must not error")
        .expect("session must still exist");
    assert!(matches!(session.state, MultipartUploadState::Aborted));
    assert!(
        session.lease_until.is_none(),
        "the lease must be cleared on abort"
    );
}

/// A `completing` session whose lease is still LIVE (`lease_until >= now`)
/// must never be aborted out from under its completer, even though it is
/// otherwise eligible by state alone.
#[tokio::test]
async fn abort_expired_completing_rejects_when_lease_still_live() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);
    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

    let live_lease = now + time::Duration::minutes(5);
    let acquired = multipart
        .acquire_complete_lease(&conn, upload_id, "completer-a", live_lease, now)
        .await
        .expect("acquire_complete_lease must not error");
    assert!(acquired, "setup: must acquire the lease");

    let aborted = multipart
        .abort_expired_completing(&conn, upload_id, now)
        .await
        .expect("abort_expired_completing must not error");
    assert!(
        !aborted,
        "a live lease must never be aborted out from under its completer"
    );

    let session = multipart
        .get(&conn, upload_id)
        .await
        .expect("get must not error")
        .expect("session must still exist");
    assert!(
        matches!(session.state, MultipartUploadState::Completing),
        "the state must be left untouched"
    );
    assert!(
        session.lease_until.is_some(),
        "the live lease must not be cleared"
    );
}

// ===========================================================================
// version_repo.rs -- VersionRepo::get_manifests
// ===========================================================================

async fn version_repo_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-vrepo-cov-{}.db", Uuid::now_v7().simple()));
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
    Arc::new(DBProvider::new(conn))
}

/// An empty `version_ids` slice short-circuits to an empty map without
/// issuing a query with an empty `IN (...)`.
#[tokio::test]
async fn get_manifests_returns_empty_map_for_empty_ids() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();

    let map = versions
        .get_manifests(&conn, &scope, &[])
        .await
        .expect("get_manifests must not error on an empty slice");
    assert!(map.is_empty());
}

/// `get_manifests` fetches every requested version's manifest text in one
/// batched call, keyed by `version_id`, and simply omits a version that has
/// no manifest row (`whole-sha256`) rather than erroring or inserting an
/// empty entry.
#[tokio::test]
async fn get_manifests_batches_lookup_across_versions() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();

    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");

    let v_with_manifest_1 = Uuid::now_v7();
    let v_with_manifest_2 = Uuid::now_v7();
    let v_without_manifest = Uuid::now_v7();
    for vid in [v_with_manifest_1, v_with_manifest_2, v_without_manifest] {
        versions
            .insert(
                &conn,
                &scope,
                &new_version(file_id, vid, VersionStatus::Available, false),
            )
            .await
            .expect("insert version");
    }
    let now = OffsetDateTime::now_utc();
    versions
        .insert_manifest(&conn, &scope, v_with_manifest_1, "manifest-1", now)
        .await
        .expect("insert manifest 1");
    versions
        .insert_manifest(&conn, &scope, v_with_manifest_2, "manifest-2", now)
        .await
        .expect("insert manifest 2");

    let map = versions
        .get_manifests(
            &conn,
            &scope,
            &[v_with_manifest_1, v_with_manifest_2, v_without_manifest],
        )
        .await
        .expect("get_manifests must not error");

    assert_eq!(
        map.len(),
        2,
        "only versions with a manifest row must appear"
    );
    assert_eq!(
        map.get(&v_with_manifest_1).map(String::as_str),
        Some("manifest-1")
    );
    assert_eq!(
        map.get(&v_with_manifest_2).map(String::as_str),
        Some("manifest-2")
    );
    assert!(
        !map.contains_key(&v_without_manifest),
        "a whole-sha256 version with no manifest row must be absent, not empty-stringed"
    );
}

// ===========================================================================
// version_repo.rs -- set_current / clear_current: both outcomes of each
// (special-attention item: a `0` from set_current is fatal to the caller's
// transaction, a `0` from clear_current is the ordinary no-op case).
// ===========================================================================

/// `set_current` on a version that genuinely exists under `(file_id,
/// version_id)` reports exactly one row affected -- the success case the
/// caller relies on to know the promotion actually happened.
#[tokio::test]
async fn set_current_returns_one_on_successful_promote() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();

    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
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

    let affected = versions
        .set_current(&conn, &scope, file_id, v1)
        .await
        .expect("set_current must not error");
    assert_eq!(affected, 1, "exactly the target row must be promoted");

    let v1_row = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .unwrap()
        .unwrap();
    assert!(v1_row.is_current);
}

/// `set_current` against a `version_id` that was never inserted (the
/// documented "deleted concurrently" case, modeled here directly as "never
/// existed") reports zero rows affected -- callers MUST treat this as fatal
/// and abort, per `VersionRepo::set_current`'s doc comment; this is the
/// invariant `store/versions.rs`'s `bind_atomic`/`finalize_version` guard
/// against.
#[tokio::test]
async fn set_current_returns_zero_when_version_missing() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();

    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    let missing = Uuid::now_v7(); // never inserted
    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");

    let affected = versions
        .set_current(&conn, &scope, file_id, missing)
        .await
        .expect("set_current must not error even when nothing matches");
    assert_eq!(
        affected, 0,
        "a set_current on a non-existent version must affect zero rows"
    );
}

/// `clear_current` reports one row affected when a version of the file is
/// actually `is_current = true`.
#[tokio::test]
async fn clear_current_returns_one_when_a_current_version_exists() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();

    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, Some(v1)))
        .await
        .expect("create file");
    versions
        .insert(
            &conn,
            &scope,
            &new_version(file_id, v1, VersionStatus::Available, true),
        )
        .await
        .expect("insert v1 as current");

    let affected = versions
        .clear_current(&conn, &scope, file_id)
        .await
        .expect("clear_current must not error");
    assert_eq!(affected, 1);

    let v1_row = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .unwrap()
        .unwrap();
    assert!(!v1_row.is_current, "the flag must actually be cleared");
}

/// `clear_current` on a file with no current version -- a brand-new file
/// whose first version has never been bound -- affects zero rows, and this
/// is documented as harmless, NOT an error the caller must react to (unlike
/// `set_current`'s `0`).
#[tokio::test]
async fn clear_current_returns_zero_when_no_current_version() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();

    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
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
        .expect("insert v1, not current");

    let affected = versions
        .clear_current(&conn, &scope, file_id)
        .await
        .expect("clear_current must not error");
    assert_eq!(
        affected, 0,
        "nothing was current, so nothing was affected -- this must not be an error"
    );
}

// ===========================================================================
// store/multipart.rs
// ===========================================================================

/// `upsert_multipart_part` on a session whose parent has already left
/// `in_progress` (here: `completed`) is rejected by the same-transaction
/// guard with the specific "not in progress" error carrying the session's
/// actual current state -- not just some generic failure.
#[tokio::test]
async fn upsert_multipart_part_rejects_when_session_not_in_progress() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);
    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

    // Drive the session to `completed` through the repo's own state
    // machine (mirrors `multipart_repo_test.rs`'s analogous setup).
    let acquired = multipart
        .acquire_complete_lease(
            &conn,
            upload_id,
            "completer-a",
            now + time::Duration::minutes(5),
            now,
        )
        .await
        .expect("acquire_complete_lease must not error");
    assert!(acquired, "setup: must acquire lease before completing");
    multipart
        .finish_complete(&conn, upload_id, "{}")
        .await
        .expect("finish_complete must not error");

    let err = store
        .upsert_multipart_part(upload_id, 1, "etag-v1", vec![1, 2, 3], 10, now)
        .await
        .expect_err("a part must not be accepted once the session is no longer in_progress");
    match err {
        DomainError::MultipartUploadNotInProgress {
            upload_id: id,
            state,
        } => {
            assert_eq!(id, upload_id);
            assert_eq!(
                state, "completed",
                "the error must report the actual current state"
            );
        }
        other => panic!("expected MultipartUploadNotInProgress, got {other:?}"),
    }

    let parts = multipart
        .list_parts(&conn, upload_id)
        .await
        .expect("list_parts must not error");
    assert!(
        parts.is_empty(),
        "no part row must be written when the guard rejects it"
    );
}

/// `abort_multipart_upload` also aborts a `completing` session whose
/// completion lease has expired (the completer died mid-assembly): it must
/// fall through the primary `in_progress -> aborted` CAS (which does not
/// match) into the `abort_expired_completing` fallback, delete the
/// session's part rows, and write the audit row -- exactly like the
/// ordinary `in_progress -> aborted` path.
#[tokio::test]
async fn abort_multipart_upload_via_expired_completing_lease_removes_parts_and_audits() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);
    let (file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

    multipart
        .upsert_part(&conn, upload_id, 1, "etag-1", vec![1, 2, 3], 10, now)
        .await
        .expect("upsert_part must not error");

    // Put the session into `completing` with an already-past lease --
    // the state `update_state("in_progress", "aborted")` cannot match
    // (it is no longer in_progress), forcing the fallback branch.
    let past_lease = now - time::Duration::minutes(1);
    multipart
        .acquire_complete_lease(&conn, upload_id, "completer-a", past_lease, now)
        .await
        .expect("acquire_complete_lease must not error");

    let tenant_id = Uuid::now_v7();
    let aborted = store
        .abort_multipart_upload(
            upload_id,
            audit_entry(tenant_id, file_id, AuditOperation::MultipartAbort),
        )
        .await
        .expect("abort_multipart_upload must not error");
    assert!(aborted, "an expired completing lease must be abortable");

    let session = multipart
        .get(&conn, upload_id)
        .await
        .unwrap()
        .expect("session still exists");
    assert!(matches!(session.state, MultipartUploadState::Aborted));

    let parts = multipart
        .list_parts(&conn, upload_id)
        .await
        .expect("list_parts must not error");
    assert!(parts.is_empty(), "part rows must be deleted on abort");

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(
        audit_rows.len(),
        1,
        "the abort must write exactly one audit row"
    );
}

/// `delete_parts_for_upload` (the standalone `Store` method, independent of
/// `abort_multipart_upload`'s own transaction) removes every part row for
/// the given upload and reports the count removed.
#[tokio::test]
async fn delete_parts_for_upload_removes_rows_and_returns_count() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);
    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

    multipart
        .upsert_part(&conn, upload_id, 1, "etag-1", vec![1], 1, now)
        .await
        .expect("upsert part 1");
    multipart
        .upsert_part(&conn, upload_id, 2, "etag-2", vec![2], 1, now)
        .await
        .expect("upsert part 2");

    let removed = store
        .delete_parts_for_upload(upload_id)
        .await
        .expect("delete_parts_for_upload must not error");
    assert_eq!(removed, 2, "both part rows must be reported as removed");

    let parts = multipart
        .list_parts(&conn, upload_id)
        .await
        .expect("list_parts must not error");
    assert!(parts.is_empty(), "the parts must actually be gone");
}

// ===========================================================================
// store/versions.rs
// ===========================================================================

/// `get_version_manifests` is a thin wrapper over
/// `VersionRepo::get_manifests` with `AccessScope::allow_all()` -- proven
/// here at the `Store` level with a mixed set of versions (some with a
/// manifest row, one without).
#[tokio::test]
async fn store_get_version_manifests_returns_batched_map() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();

    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");
    let v1 = Uuid::now_v7();
    let v2 = Uuid::now_v7();
    for vid in [v1, v2] {
        versions
            .insert(
                &conn,
                &scope,
                &new_version(file_id, vid, VersionStatus::Available, false),
            )
            .await
            .expect("insert version");
    }
    versions
        .insert_manifest(
            &conn,
            &scope,
            v1,
            "manifest-for-v1",
            OffsetDateTime::now_utc(),
        )
        .await
        .expect("insert manifest");

    let map = store
        .get_version_manifests(&[v1, v2])
        .await
        .expect("get_version_manifests must not error");
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(&v1).map(String::as_str), Some("manifest-for-v1"));
    assert!(!map.contains_key(&v2));
}

/// `bind_atomic` returns `Ok(false)` -- not an error -- when the CAS's
/// `expected_content_id` does not match the file's actual `content_id`, and
/// in that case must never touch `file_versions` or write an audit row: the
/// whole promote sequence is short-circuited before it starts.
#[tokio::test]
async fn bind_atomic_returns_false_on_cas_mismatch_without_touching_versions() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
    let wrongly_expected = Uuid::now_v7(); // the file's actual content_id is None

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
            Some(wrongly_expected),
            v1,
            OffsetDateTime::now_utc(),
            audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
        )
        .await
        .expect("a CAS mismatch must be reported, not an error");
    assert!(!bound);

    let v1_row = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !v1_row.is_current,
        "a failed CAS must never promote the version"
    );

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert!(
        audit_rows.is_empty(),
        "a failed CAS must never write an audit row"
    );
}

/// The events-aware `bind_atomic_with_event` short-circuits identically on a
/// CAS mismatch: `Ok(false)`, no version touched, and no event enqueued even
/// though the caller passed one.
#[tokio::test]
async fn bind_atomic_with_event_returns_false_on_cas_mismatch_enqueues_no_event() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let versions = VersionRepo::new();

    let tenant_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let v1 = Uuid::now_v7();
    let wrongly_expected = Uuid::now_v7();

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

    let bound = store
        .bind_atomic_with_event(
            &scope,
            file_id,
            Some(wrongly_expected),
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
        .expect("a CAS mismatch must be reported, not an error");
    assert!(!bound);

    let events = store.list_file_events(file_id).await.unwrap();
    assert!(
        events.is_empty(),
        "no event may be enqueued on a failed CAS"
    );
    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert!(audit_rows.is_empty());
}

// ===========================================================================
// store/files.rs
// ===========================================================================

/// The plain (non-event) `delete_file` removes the file row, cascades its
/// version, and writes an audit row -- exercised directly since every
/// domain-layer caller currently goes through `delete_file_with_event`
/// instead, leaving this variant itself unexercised.
#[tokio::test]
async fn delete_file_plain_removes_row_cascades_version_and_audits() {
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
            &new_version(file_id, version_id, VersionStatus::Available, false),
        )
        .await
        .expect("insert version");

    let removed = store
        .delete_file(
            &scope,
            file_id,
            audit_entry(tenant_id, file_id, AuditOperation::DeleteFile),
        )
        .await
        .expect("delete_file must not error");
    assert!(removed, "the file row must be found and removed");

    assert!(files.get(&conn, &scope, file_id).await.unwrap().is_none());
    assert!(
        versions
            .get(&conn, &scope, file_id, version_id)
            .await
            .unwrap()
            .is_none(),
        "the version must cascade-delete with its parent file"
    );

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(
        audit_rows.len(),
        1,
        "the delete must write exactly one audit row"
    );
}

/// `delete_file` on a `file_id` that does not exist reports `false` and
/// writes no audit row -- the guard on the transaction's `if removed` branch.
#[tokio::test]
async fn delete_file_plain_returns_false_for_missing_file() {
    let (store, _db) = build_store().await;
    let scope = AccessScope::allow_all();
    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();

    let removed = store
        .delete_file(
            &scope,
            file_id,
            audit_entry(tenant_id, file_id, AuditOperation::DeleteFile),
        )
        .await
        .expect("delete_file must not error on a missing file");
    assert!(!removed);

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert!(audit_rows.is_empty(), "no audit row for a no-op delete");
}

/// `create_file_with_event` with `event: Some(..)` inserts the file row and
/// enqueues the given event in the same transaction as the create's audit
/// row.
#[tokio::test]
async fn create_file_with_event_some_enqueues_event() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();

    let tenant_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let new = new_file_req(owner_id, vec![]);

    store
        .create_file_with_event(
            &new,
            file_id,
            tenant_id,
            now,
            audit_entry(tenant_id, file_id, AuditOperation::Create),
            Some(file_event(tenant_id, owner_id, file_id, "file.created")),
        )
        .await
        .expect("create_file_with_event must not error");

    let file = files
        .get(&conn, &scope, file_id)
        .await
        .unwrap()
        .expect("file row must exist");
    assert_eq!(file.tenant_id, tenant_id);

    let events = store.list_file_events(file_id).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "file.created");

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(audit_rows.len(), 1);
}

// ===========================================================================
// store/traits.rs -- CleanupStore / MultipartStore delegation
// ===========================================================================

/// `CleanupStore::list_metadata_for_files`, called through the trait object
/// (not the `Store` inherent method directly), forwards correctly and
/// short-circuits an empty `file_ids` slice to an empty map.
#[tokio::test]
async fn cleanup_store_trait_list_metadata_for_files_empty() {
    let (store, _db) = build_store().await;
    let map = CleanupStore::list_metadata_for_files(&store, &[])
        .await
        .expect("must not error");
    assert!(map.is_empty());
}

/// `MultipartStore::get_version_manifest`, called through the trait object,
/// forwards to `Store::get_version_manifest` and reports `None` for a
/// version with no manifest row.
#[tokio::test]
async fn multipart_store_trait_get_version_manifest_none_for_unknown_version() {
    let (store, _db) = build_store().await;
    let manifest = MultipartStore::get_version_manifest(&store, Uuid::now_v7())
        .await
        .expect("must not error");
    assert!(manifest.is_none());
}

// ===========================================================================
// metadata_repo.rs -- empty-input short-circuits
// ===========================================================================

/// `list_for_files` with an empty `file_ids` slice returns an empty map
/// without issuing an `IN ()` query.
#[tokio::test]
async fn metadata_list_for_files_returns_empty_map_for_empty_ids() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let metadata = MetadataRepo::new();
    let scope = AccessScope::allow_all();

    let map = metadata
        .list_for_files(&conn, &scope, &[])
        .await
        .expect("list_for_files must not error on an empty slice");
    assert!(map.is_empty());
}

/// `delete_keys` with an empty `keys` slice is a no-op reporting `0` rows
/// affected, without issuing a delete statement at all.
#[tokio::test]
async fn metadata_delete_keys_returns_zero_for_empty_keys() {
    let db = version_repo_db().await;
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let metadata = MetadataRepo::new();
    let scope = AccessScope::allow_all();

    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant_id, None))
        .await
        .expect("create file");
    metadata
        .insert_many(
            &conn,
            &scope,
            file_id,
            &[("k1".to_owned(), "v1".to_owned())],
            OffsetDateTime::now_utc(),
        )
        .await
        .expect("seed one metadata entry");

    let removed = metadata
        .delete_keys(&conn, &scope, file_id, &[])
        .await
        .expect("delete_keys must not error on an empty slice");
    assert_eq!(removed, 0);

    let entries = metadata.list(&conn, &scope, file_id).await.unwrap();
    assert_eq!(entries.len(), 1, "an empty key list must delete nothing");
}
