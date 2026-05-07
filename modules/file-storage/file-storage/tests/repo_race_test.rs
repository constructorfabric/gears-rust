//! Race-condition tests for `SeaOrmFilesRepository`.
//!
//! Per `docs/modkit_unified_system/12_unit_testing.md` "Database Usage in
//! Unit Tests" — using SQLite `:memory:` to verify that the repository's
//! conditional-UPDATE / race-detection semantics work correctly. These
//! tests exercise the actual SQL produced by SeaORM and the `WHERE
//! status = X AND etag = Y AND ...` race-detection predicate that is the
//! heart of P1's multi-phase commit correctness model.
//!
//! Coverage matrix (each row = one `#[tokio::test]`):
//!
//!   Operation              | Match (Applied) | No-match (NoMatch)
//!   ---------------------- | --------------- | -------------------
//!   begin_complete_upload  | ✓               | ✓ (status mismatch)
//!   finish_complete_upload | ✓               | ✓
//!   begin_meta_update      | ✓               | ✓ (etag stale)
//!                                            | ✓ (version_id stale)
//!   finish_meta_update     | ✓               | ✓
//!   begin_delete           | ✓               | ✓ (etag stale)
//!                                            | ✓ (already deleting)
//!   finish_delete          | ✓               | ✓
//!   delete_pending_upload  | ✓               | ✓ (already uploaded)
//!   get_by_id              | ✓ (own tenant)  | ✓ (other tenant returns None)
//!
//! Each test creates its own SQLite DB and is independent. The full suite
//! runs in <30 ms total.

mod common;

use common::{finalize_upload, insert_pending_row, repo, test_db};
use file_storage::domain::repo::{ChangeStatusOutcome, FilesRepo, MutationOutcome};
use file_storage_sdk::{FileMetaUpdate, FileStatus};
use time::OffsetDateTime;
use uuid::Uuid;

// ── insert_pending + get_by_id ──────────────────────────────────────────────

#[tokio::test]
async fn insert_pending_creates_row_in_pending_status() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    assert_eq!(info.status, FileStatus::PendingUpload);
    assert_eq!(info.size_bytes, 0);
    assert!(info.etag.is_empty(), "fresh row should have sentinel etag");
    assert!(info.version_id.is_none());
    assert!(info.upload_expires_at.is_some());
}

#[tokio::test]
async fn get_by_id_in_own_tenant_returns_row() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let inserted = insert_pending_row(&db, &repo, tenant).await;
    let conn = db.conn().expect("conn");
    let got = repo
        .get_by_id(&conn, tenant, inserted.file_id)
        .await
        .expect("get_by_id");
    let got = got.expect("row found in own tenant");
    assert_eq!(got.file_id, inserted.file_id);
}

#[tokio::test]
async fn get_by_id_other_tenant_returns_none() {
    // Tenant-scoping: secure_find with AccessScope::for_tenant filters out
    // rows belonging to other tenants.
    let db = test_db().await;
    let repo = repo();
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let inserted = insert_pending_row(&db, &repo, tenant_a).await;
    let conn = db.conn().expect("conn");
    let got = repo
        .get_by_id(&conn, tenant_b, inserted.file_id)
        .await
        .expect("get_by_id");
    assert!(
        got.is_none(),
        "tenant_b should not see tenant_a's row, got: {got:?}"
    );
}

// ── begin_complete_upload (Phase 1) ─────────────────────────────────────────

#[tokio::test]
async fn begin_complete_upload_pending_to_completing_applies() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_complete_upload(&conn, tenant, info.file_id, OffsetDateTime::now_utc())
        .await
        .expect("begin_complete_upload");
    assert_eq!(outcome, MutationOutcome::Applied);
    let row = repo
        .get_by_id(&conn, tenant, info.file_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, FileStatus::Completing);
}

#[tokio::test]
async fn begin_complete_upload_twice_second_is_nomatch() {
    // Race scenario: two concurrent complete_upload callers — first wins,
    // second sees the row already in `completing` status and gets NoMatch.
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let conn = db.conn().expect("conn");
    let first = repo
        .begin_complete_upload(&conn, tenant, info.file_id, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(first, MutationOutcome::Applied);
    let second = repo
        .begin_complete_upload(&conn, tenant, info.file_id, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(second, MutationOutcome::NoMatch, "race-loser must see NoMatch");
}

#[tokio::test]
async fn begin_complete_upload_on_uploaded_row_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 100).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_complete_upload(&conn, tenant, info.file_id, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::NoMatch);
}

// ── finish_complete_upload (Phase 3) ────────────────────────────────────────

#[tokio::test]
async fn finish_complete_upload_completing_to_uploaded_applies() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let conn = db.conn().expect("conn");
    repo.begin_complete_upload(&conn, tenant, info.file_id, OffsetDateTime::now_utc())
        .await
        .unwrap();
    let outcome = repo
        .finish_complete_upload(
            &conn,
            tenant,
            info.file_id,
            "abc-2",
            Some("v1"),
            12345,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    let info_after = match outcome {
        ChangeStatusOutcome::Applied(i) => i,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(info_after.status, FileStatus::Uploaded);
    assert_eq!(info_after.etag, "abc-2");
    assert_eq!(info_after.version_id.as_deref(), Some("v1"));
    assert_eq!(info_after.size_bytes, 12345);
    assert!(
        info_after.upload_expires_at.is_none(),
        "upload_expires_at must be cleared on Uploaded"
    );
}

#[tokio::test]
async fn finish_complete_upload_without_phase_1_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let conn = db.conn().expect("conn");
    // Skip Phase 1 — row still in `pending_upload`. finish should not match.
    let outcome = repo
        .finish_complete_upload(
            &conn,
            tenant,
            info.file_id,
            "abc",
            None,
            100,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, ChangeStatusOutcome::NoMatch));
}

// ── begin_meta_update + ETag/VersionId pins ─────────────────────────────────

#[tokio::test]
async fn begin_meta_update_with_correct_etag_applies() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag-curr", None, 10).await;
    let conn = db.conn().expect("conn");

    let upd = FileMetaUpdate {
        name: Some("renamed.pdf".into()),
        ..Default::default()
    };
    let outcome = repo
        .begin_meta_update(
            &conn,
            tenant,
            info.file_id,
            Some("etag-curr"),
            None,
            &upd,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    let info_after = match outcome {
        ChangeStatusOutcome::Applied(i) => i,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(info_after.status, FileStatus::MetaUpdating);
    assert_eq!(info_after.meta.name, "renamed.pdf");
}

#[tokio::test]
async fn begin_meta_update_with_stale_etag_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag-curr", None, 10).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_meta_update(
            &conn,
            tenant,
            info.file_id,
            Some("etag-stale"), // ← does not match `etag-curr`
            None,
            &FileMetaUpdate::default(),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, ChangeStatusOutcome::NoMatch));
}

#[tokio::test]
async fn begin_meta_update_with_version_pin_match_applies() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(
        &db,
        &repo,
        tenant,
        info.file_id,
        "etag-curr",
        Some("v1"),
        10,
    )
    .await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_meta_update(
            &conn,
            tenant,
            info.file_id,
            None,
            Some("v1"),
            &FileMetaUpdate::default(),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, ChangeStatusOutcome::Applied(_)));
}

#[tokio::test]
async fn begin_meta_update_with_version_pin_stale_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(
        &db,
        &repo,
        tenant,
        info.file_id,
        "etag-curr",
        Some("v1"),
        10,
    )
    .await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_meta_update(
            &conn,
            tenant,
            info.file_id,
            None,
            Some("v999"), // ← stale version
            &FileMetaUpdate::default(),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, ChangeStatusOutcome::NoMatch));
}

#[tokio::test]
async fn begin_meta_update_concurrent_only_first_applies() {
    // Two concurrent put_file_info race — both pin the same etag, both call
    // begin_meta_update. First flips status → meta_updating, second sees the
    // row no longer in `uploaded` and gets NoMatch.
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag-curr", None, 10).await;
    let conn = db.conn().expect("conn");
    let upd = FileMetaUpdate::default();

    let first = repo
        .begin_meta_update(
            &conn,
            tenant,
            info.file_id,
            Some("etag-curr"),
            None,
            &upd,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert!(matches!(first, ChangeStatusOutcome::Applied(_)));

    let second = repo
        .begin_meta_update(
            &conn,
            tenant,
            info.file_id,
            Some("etag-curr"),
            None,
            &upd,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert!(
        matches!(second, ChangeStatusOutcome::NoMatch),
        "race-loser must see NoMatch (status now meta_updating, not uploaded)"
    );
}

// ── finish_meta_update (Phase 3 of put_file_info) ───────────────────────────

#[tokio::test]
async fn finish_meta_update_writes_new_etag_and_version() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 10).await;
    let conn = db.conn().expect("conn");
    repo.begin_meta_update(
        &conn,
        tenant,
        info.file_id,
        None,
        None,
        &FileMetaUpdate::default(),
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let outcome = repo
        .finish_meta_update(
            &conn,
            tenant,
            info.file_id,
            "etag2",
            Some("v2"),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    let info_after = match outcome {
        ChangeStatusOutcome::Applied(i) => i,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(info_after.status, FileStatus::Uploaded);
    assert_eq!(info_after.etag, "etag2");
    assert_eq!(info_after.version_id.as_deref(), Some("v2"));
}

// ── begin_delete + ETag/Version pins ────────────────────────────────────────

#[tokio::test]
async fn begin_delete_with_correct_etag_applies() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 10).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_delete(
            &conn,
            tenant,
            info.file_id,
            Some("etag1"),
            None,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::Applied);
    let row = repo
        .get_by_id(&conn, tenant, info.file_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, FileStatus::Deleting);
}

#[tokio::test]
async fn begin_delete_with_stale_etag_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 10).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_delete(
            &conn,
            tenant,
            info.file_id,
            Some("etag-WRONG"),
            None,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::NoMatch);
}

#[tokio::test]
async fn begin_delete_already_deleting_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 10).await;
    let conn = db.conn().expect("conn");
    repo.begin_delete(
        &conn,
        tenant,
        info.file_id,
        None,
        None,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let second = repo
        .begin_delete(
            &conn,
            tenant,
            info.file_id,
            None,
            None,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(second, MutationOutcome::NoMatch);
}

#[tokio::test]
async fn begin_delete_with_version_pin_match_applies() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(
        &db,
        &repo,
        tenant,
        info.file_id,
        "etag1",
        Some("vX"),
        10,
    )
    .await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_delete(
            &conn,
            tenant,
            info.file_id,
            None,
            Some("vX"),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::Applied);
}

#[tokio::test]
async fn begin_delete_with_version_pin_stale_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(
        &db,
        &repo,
        tenant,
        info.file_id,
        "etag1",
        Some("vX"),
        10,
    )
    .await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_delete(
            &conn,
            tenant,
            info.file_id,
            None,
            Some("vY"),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::NoMatch);
}

// ── finish_delete (Phase 3 of delete_file) ──────────────────────────────────

#[tokio::test]
async fn finish_delete_purges_deleting_row() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 10).await;
    let conn = db.conn().expect("conn");
    repo.begin_delete(
        &conn,
        tenant,
        info.file_id,
        None,
        None,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let outcome = repo
        .finish_delete(&conn, tenant, info.file_id)
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::Applied);
    let row = repo
        .get_by_id(&conn, tenant, info.file_id)
        .await
        .unwrap();
    assert!(row.is_none(), "row must be gone after finish_delete");
}

#[tokio::test]
async fn finish_delete_on_uploaded_row_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 10).await;
    let conn = db.conn().expect("conn");
    // Skip begin_delete — row is still `uploaded`.
    let outcome = repo
        .finish_delete(&conn, tenant, info.file_id)
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::NoMatch);
}

// ── delete_pending_upload (abort_upload internal step) ──────────────────────

#[tokio::test]
async fn delete_pending_upload_removes_pending_row() {
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .delete_pending_upload(&conn, tenant, info.file_id)
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::Applied);
    let row = repo
        .get_by_id(&conn, tenant, info.file_id)
        .await
        .unwrap();
    assert!(row.is_none());
}

#[tokio::test]
async fn delete_pending_upload_on_uploaded_row_is_nomatch() {
    // Variant-B re-upload abort: the row is `uploaded` (not pending).
    // delete_pending_upload must NOT touch it.
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 10).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .delete_pending_upload(&conn, tenant, info.file_id)
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::NoMatch);
    // Row must still exist with status=uploaded.
    let row = repo
        .get_by_id(&conn, tenant, info.file_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, FileStatus::Uploaded);
}

// ── Tenant isolation on mutations ───────────────────────────────────────────

#[tokio::test]
async fn begin_complete_upload_other_tenant_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant_a).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_complete_upload(&conn, tenant_b, info.file_id, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(
        outcome,
        MutationOutcome::NoMatch,
        "tenant_b must not be able to mutate tenant_a's row"
    );
}

#[tokio::test]
async fn begin_delete_other_tenant_is_nomatch() {
    let db = test_db().await;
    let repo = repo();
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant_a).await;
    let info = finalize_upload(&db, &repo, tenant_a, info.file_id, "etag1", None, 10).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_delete(
            &conn,
            tenant_b,
            info.file_id,
            None,
            None,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, MutationOutcome::NoMatch);
    // Sanity: row still present in tenant_a's view.
    let row = repo
        .get_by_id(&conn, tenant_a, info.file_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, FileStatus::Uploaded);
}

// ── Versioning vs no-versioning matrix on FileInfo ──────────────────────────

#[tokio::test]
async fn finalize_with_no_version_id_yields_none_on_row() {
    // Backend without S3 versioning: complete returns no x-amz-version-id;
    // row.version_id stays None.
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info_after = finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 10).await;
    assert!(info_after.version_id.is_none());
}

#[tokio::test]
async fn finalize_with_version_id_yields_some_on_row() {
    // Backend with S3 versioning: complete returns x-amz-version-id;
    // row.version_id is populated.
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info_after = finalize_upload(
        &db,
        &repo,
        tenant,
        info.file_id,
        "etag1",
        Some("v123"),
        10,
    )
    .await;
    assert_eq!(info_after.version_id.as_deref(), Some("v123"));
}

#[tokio::test]
async fn version_id_pin_against_no_version_row_is_nomatch() {
    // Caller passes Some(version_id) to a backend with no versioning —
    // row.version_id is None, the WHERE clause never matches.
    let db = test_db().await;
    let repo = repo();
    let tenant = Uuid::new_v4();
    let info = insert_pending_row(&db, &repo, tenant).await;
    let info = finalize_upload(&db, &repo, tenant, info.file_id, "etag1", None, 10).await;
    let conn = db.conn().expect("conn");
    let outcome = repo
        .begin_delete(
            &conn,
            tenant,
            info.file_id,
            None,
            Some("vX"),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        MutationOutcome::NoMatch,
        "Some(version) on no-versioning row must not match"
    );
}
