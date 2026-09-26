//! Repo-level tests for `MultipartRepo`'s recently changed mechanics:
//! - `acquire_complete_lease`'s `expires_at > now` fencing.
//! - `upsert_part`'s single-statement on-conflict upsert plus its
//!   same-transaction "parent still `in_progress`" guard.
//! - `has_active_for_file`'s `LIMIT 1` existence check.
//!
//! Mirrors `tests/version_repo_test.rs`: a real SQLite DB with the full
//! migration applied, `DBProvider::conn()` for a `DBRunner`, and
//! `AccessScope::allow_all()` (this table has no tenant column -- see
//! `multipart_repo.rs`'s module doc comment).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::domain::multipart::MultipartUploadState;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::{FileRepo, MultipartRepo};
use file_storage_sdk::{File, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file SQLite DB (mirrors `version_repo_test.rs::db()`) -- a
/// bare `sqlite::memory:` gives each pooled connection its own empty DB.
async fn db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-multipart-repo-test-{}.db",
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
    Arc::new(DBProvider::new(conn))
}

fn new_file(file_id: Uuid, tenant_id: Uuid) -> File {
    let now = OffsetDateTime::now_utc();
    File {
        file_id,
        tenant_id,
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "upload.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        content_id: None,
        meta_version: 0,
        created_at: now,
        last_modified_at: now,
    }
}

/// Seed one `files` row and one `multipart_uploads` session
/// (`state = in_progress`) for it, returning `(file_id, upload_id)`.
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
        .create(conn, &scope, &new_file(file_id, tenant_id))
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
            Some("mem"),
            Some(&format!("/{file_id}/{version_id}")),
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

// -- acquire_complete_lease: expires_at > now fencing -----------------------

/// A live session (`expires_at` still in the future) can have its completion
/// lease acquired: the CAS succeeds and the row visibly moves to
/// `completing` with the given owner/lease_until.
#[tokio::test]
async fn acquire_complete_lease_succeeds_for_live_session() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);

    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

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
    assert!(acquired, "a live in_progress session must accept the lease");

    let session = multipart
        .get(&conn, upload_id)
        .await
        .expect("get must not error")
        .expect("session must exist");
    assert!(matches!(session.state, MultipartUploadState::Completing));
    assert!(
        session.lease_until.is_some(),
        "lease_until must be recorded on a successful acquire"
    );
}

/// A session whose `expires_at` has already passed cannot have its
/// completion lease acquired, even though its `state` is still
/// `in_progress` -- the CAS's `expires_at > now` fence must reject it, and
/// the row must be left completely unchanged.
#[tokio::test]
async fn acquire_complete_lease_rejects_expired_session() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    // The session's own expires_at is in the past relative to `now`, even
    // though its persisted state is still `in_progress`.
    let expires_at = now - time::Duration::hours(1);

    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

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
    assert!(
        !acquired,
        "an expired session must never accept a fresh completion lease"
    );

    let session = multipart
        .get(&conn, upload_id)
        .await
        .expect("get must not error")
        .expect("session must still exist");
    assert!(
        matches!(session.state, MultipartUploadState::InProgress),
        "a rejected acquire must leave the state untouched"
    );
    assert!(
        session.lease_until.is_none(),
        "a rejected acquire must not write a lease_until"
    );
}

// -- finish_complete: lease_owner fencing (DBS-05) --------------------------

/// A foreign/taken-over lease cannot transition the session to `completed`:
/// once the lease is held under `"completer-a"`, a `finish_complete` call
/// asserting a DIFFERENT owner (e.g. `"completer-b"`, representing a
/// takeover that reassigned `lease_owner`) must lose its CAS -- the session
/// stays `completing`, with no `complete_result` written and its lease
/// untouched, exactly as if the call had never happened. Mirrors
/// `release_complete_lease`'s and `acquire_complete_lease`'s own
/// owner/expiry fencing -- `finish_complete` was the one CAS in this state
/// machine missing an owner predicate (see its doc comment for the one
/// deliberate exception: `Store::finalize_multipart_version`'s own embedded
/// call passes `None` instead of a foreign owner, which is a different case
/// from this test).
#[tokio::test]
async fn finish_complete_rejects_foreign_lease_owner() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);

    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

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
    assert!(acquired, "setup: must acquire the lease before completing");

    let finished = multipart
        .finish_complete(&conn, upload_id, Some("completer-b"), "{\"snapshot\":true}")
        .await
        .expect("finish_complete must not error even when its CAS loses");
    assert!(
        !finished,
        "a foreign lease_owner must not be able to complete the session"
    );

    let session = multipart
        .get(&conn, upload_id)
        .await
        .expect("get must not error")
        .expect("session must still exist");
    assert!(
        matches!(session.state, MultipartUploadState::Completing),
        "a rejected finish_complete must leave state untouched"
    );
    assert!(
        session.complete_result.is_none(),
        "a rejected finish_complete must not persist a complete_result snapshot"
    );
    assert!(
        session.lease_until.is_some(),
        "a rejected finish_complete must not clear the (still legitimately held) lease"
    );
}

/// The positive counterpart: the CURRENT lease owner's own `finish_complete`
/// call succeeds -- the `lease_owner` predicate added for DBS-05 only
/// rejects a mismatched owner, never the legitimate holder.
#[tokio::test]
async fn finish_complete_accepts_matching_lease_owner() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);

    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

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
    assert!(acquired, "setup: must acquire the lease before completing");

    let finished = multipart
        .finish_complete(&conn, upload_id, Some("completer-a"), "{\"snapshot\":true}")
        .await
        .expect("finish_complete must not error");
    assert!(
        finished,
        "the current, matching lease owner must be able to complete the session"
    );

    let session = multipart
        .get(&conn, upload_id)
        .await
        .expect("get must not error")
        .expect("session must still exist");
    assert!(matches!(session.state, MultipartUploadState::Completed));
    assert_eq!(
        session.complete_result.as_deref(),
        Some("{\"snapshot\":true}")
    );
}

// -- upsert_part: on-conflict upsert + in_progress guard --------------------

/// Reporting the same part number twice updates the existing row in place
/// (new etag/hash/size/timestamp) instead of creating a second row or
/// failing on the primary key.
#[tokio::test]
async fn upsert_part_updates_existing_row_on_second_report() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);

    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

    let wrote_first = multipart
        .upsert_part(&conn, upload_id, 1, "etag-v1", vec![1, 2, 3], 10, now)
        .await
        .expect("first upsert_part must not error");
    assert!(wrote_first);

    let later = now + time::Duration::seconds(30);
    let wrote_second = multipart
        .upsert_part(&conn, upload_id, 1, "etag-v2", vec![9, 9, 9], 20, later)
        .await
        .expect("second upsert_part must not error");
    assert!(wrote_second);

    let parts = multipart
        .list_parts(&conn, upload_id)
        .await
        .expect("list_parts must not error");
    assert_eq!(
        parts.len(),
        1,
        "the same part_number must never produce a second row"
    );
    let part = &parts[0];
    assert_eq!(
        part.backend_etag, "etag-v2",
        "etag must reflect the latest report"
    );
    assert_eq!(
        part.part_hash,
        vec![9, 9, 9],
        "hash must reflect the latest report"
    );
    assert_eq!(part.size, 20, "size must reflect the latest report");
    assert_eq!(
        part.uploaded_at, later,
        "uploaded_at must reflect the latest report, not the original"
    );
}

/// A part reported against a session whose parent is not `in_progress`
/// (already `completed`, in this case) is rejected by the same-transaction
/// guard: `upsert_part` returns `false` and no part row is written.
#[tokio::test]
async fn upsert_part_rejects_when_parent_not_in_progress() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);

    let (_file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

    // Move the session out of `in_progress` using the repo's own state
    // machine (mirrors `finish_complete`'s terminal transition), rather than
    // reaching into the entity layer directly.
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
    assert!(acquired, "setup: must acquire the lease before completing");
    let finished = multipart
        .finish_complete(&conn, upload_id, Some("completer-a"), "{}")
        .await
        .expect("finish_complete must not error");
    assert!(finished, "setup: session must reach completed");

    let wrote = multipart
        .upsert_part(&conn, upload_id, 1, "etag-v1", vec![1, 2, 3], 10, now)
        .await
        .expect("upsert_part must not error even when the guard rejects it");
    assert!(
        !wrote,
        "a part must not be written once the parent session left in_progress"
    );

    let parts = multipart
        .list_parts(&conn, upload_id)
        .await
        .expect("list_parts must not error");
    assert!(
        parts.is_empty(),
        "no part row must exist when the in_progress guard rejected the write"
    );
}

// -- has_active_for_file: LIMIT 1 existence check ---------------------------

/// `has_active_for_file` reports `true` while a session for the file is
/// still `in_progress`.
#[tokio::test]
async fn has_active_for_file_true_when_live_session_exists() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);

    let (file_id, _upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

    let has_active = multipart
        .has_active_for_file(&conn, file_id)
        .await
        .expect("has_active_for_file must not error");
    assert!(has_active);
}

/// `has_active_for_file` reports `false` for a file with no multipart
/// sessions at all.
#[tokio::test]
async fn has_active_for_file_false_when_no_session_exists() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let files = FileRepo::new();
    let scope = AccessScope::allow_all();
    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant_id))
        .await
        .expect("create parent file");

    let has_active = multipart
        .has_active_for_file(&conn, file_id)
        .await
        .expect("has_active_for_file must not error");
    assert!(!has_active);
}

/// `has_active_for_file` reports `false` once the file's only session
/// has moved to a terminal state (`completed`), even though the row itself
/// still exists.
#[tokio::test]
async fn has_active_for_file_false_when_session_is_terminal() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);

    let (file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

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
    assert!(acquired, "setup: must acquire the lease before completing");
    let finished = multipart
        .finish_complete(&conn, upload_id, Some("completer-a"), "{}")
        .await
        .expect("finish_complete must not error");
    assert!(finished, "setup: session must reach completed");

    let has_active = multipart
        .has_active_for_file(&conn, file_id)
        .await
        .expect("has_active_for_file must not error");
    assert!(!has_active, "a completed session must not count as active");
}

/// `has_active_for_file` reports `true` while a session is `completing`
/// under a live lease -- the exact window the P2 2.8 orphan-file guard
/// exists to protect (a completer is assembling the final object right now).
#[tokio::test]
async fn has_active_for_file_true_when_completing_with_live_lease() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);

    let (file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

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
    assert!(acquired, "setup: must acquire the lease before completing");

    let has_active = multipart
        .has_active_for_file(&conn, file_id)
        .await
        .expect("has_active_for_file must not error");
    assert!(
        has_active,
        "a completing session under a live lease must count as active"
    );
}

/// `has_active_for_file` reports `true` for a `completing` session even once
/// its lease has lapsed -- reaping a stuck lease is `sweep_expired_multipart`'s
/// job, not this guard's, so it must keep blocking until that CAS lands.
#[tokio::test]
async fn has_active_for_file_true_when_completing_with_expired_lease() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::multipart_upload::{
        Column as UploadColumn, Entity as UploadEntity,
    };

    let db = db().await;
    let conn = db.conn().expect("conn");
    let multipart = MultipartRepo::new();
    let now = OffsetDateTime::now_utc();
    let expires_at = now + time::Duration::hours(1);

    let (file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;

    let past = now - time::Duration::hours(1);
    UploadEntity::update_many()
        .col_expr(UploadColumn::State, Expr::value("completing"))
        .col_expr(UploadColumn::LeaseUntil, Expr::value(Some(past)))
        .col_expr(
            UploadColumn::LeaseOwner,
            Expr::value(Some("stale-completer".to_owned())),
        )
        .filter(UploadColumn::UploadId.eq(upload_id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate session into expired-lease completing state");

    let has_active = multipart
        .has_active_for_file(&conn, file_id)
        .await
        .expect("has_active_for_file must not error");
    assert!(
        has_active,
        "a completing session with an expired lease still counts as active \
         until sweep_expired_multipart reaps it"
    );
}

/// `MultipartRepo::list_expired` must cap its result at `limit` rows -- the
/// cleanup sweep's expired-multipart phase used to run this query with no
/// bound at all, materializing an entire backlog of stale sessions in one
/// sweep pass -- and must order deterministically: `(expires_at, upload_id)`
/// ascending, not merely `expires_at` (which alone leaves ties unordered).
///
/// Seeds four expired `in_progress` sessions under one file, one more than
/// `limit = 3`: `a` strictly the longest-expired, `d` strictly the most
/// recently expired (must be excluded), and `b`/`c` sharing `a`'s successor
/// `expires_at` with `b`'s `upload_id` numerically below `c`'s (constructed
/// via `Uuid::from_u128`, not `Uuid::now_v7`, so the tiebreak assertion does
/// not depend on wall-clock generation order).
#[tokio::test]
async fn list_expired_caps_at_limit_and_orders_deterministically() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let multipart = MultipartRepo::new();
    let scope = AccessScope::allow_all();

    let file_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant_id))
        .await
        .expect("create parent file");

    let now = OffsetDateTime::now_utc();
    let id_a = Uuid::from_u128(1);
    let id_b = Uuid::from_u128(11);
    let id_c = Uuid::from_u128(12);
    let id_d = Uuid::from_u128(99);

    for (upload_id, expires_at) in [
        (id_a, now - time::Duration::hours(3)),
        (id_c, now - time::Duration::hours(2)),
        (id_b, now - time::Duration::hours(2)),
        (id_d, now - time::Duration::hours(1)),
    ] {
        multipart
            .create(
                &conn,
                upload_id,
                file_id,
                Uuid::now_v7(),
                "backend-handle",
                Some("mem"),
                Some("/x"),
                "application/octet-stream",
                100,
                50,
                false,
                expires_at,
                now,
            )
            .await
            .expect("create expired session");
    }

    let rows = multipart
        .list_expired(&conn, now, 3)
        .await
        .expect("list_expired must not error");

    assert_eq!(
        rows.len(),
        3,
        "limit = 3 over 4 eligible candidates must return exactly 3 rows"
    );
    assert_eq!(
        rows.iter().map(|s| s.upload_id).collect::<Vec<_>>(),
        vec![id_a, id_b, id_c],
        "rows must be ordered (expires_at, upload_id) ascending -- a first \
         (longest-expired), then b before c (same expires_at, b's upload_id \
         is smaller), and d (most recently expired) excluded by the limit"
    );
}
