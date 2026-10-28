//! Repo-level tests for `MultipartRepo`: completion-lease fencing and `has_active_for_file`.
//! Real SQLite DB with the full migration applied.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::domain::multipart::MultipartUploadState;
use file_storage::infra::storage::repo::{FileRepo, MultipartRepo};
use file_storage_sdk::{File, OwnerKind};

mod common;

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

async fn db() -> Arc<DBProvider<DbError>> {
    common::build_db("multipart-repo").await
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
            "mem",
            &format!("/{file_id}/{version_id}"),
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

/// Takes the completion lease as `owner` for five minutes.
async fn acquire<C: toolkit_db::secure::DBRunner>(
    conn: &C,
    multipart: &MultipartRepo,
    upload_id: Uuid,
    owner: &str,
    now: OffsetDateTime,
) -> bool {
    multipart
        .acquire_complete_lease(
            conn,
            upload_id,
            owner,
            now + time::Duration::minutes(5),
            now,
        )
        .await
        .expect("acquire_complete_lease must not error")
}

/// `acquire_complete_lease`: a live `in_progress` session takes the lease (-> `completing`, with
/// `lease_until` recorded); a session whose `expires_at` has passed cannot be leased even while
/// still `in_progress`, and the rejected acquire leaves the row unchanged.
#[tokio::test]
async fn acquire_complete_lease_accepts_live_and_rejects_expired_session() {
    // (row, session expires_at offset, expected acquired)
    for (row, expires_in, expected) in [
        ("live session", time::Duration::hours(1), true),
        ("expired session", -time::Duration::hours(1), false),
    ] {
        let db = db().await;
        let conn = db.conn().expect("conn");
        let multipart = MultipartRepo::new();
        let now = OffsetDateTime::now_utc();
        let (_file_id, upload_id) = seed_session(&conn, &multipart, now + expires_in, now).await;

        let acquired = acquire(&conn, &multipart, upload_id, "completer-a", now).await;
        assert_eq!(acquired, expected, "{row}: acquired");

        let session = multipart
            .get(&conn, upload_id)
            .await
            .expect("get must not error")
            .expect("session must exist");
        let expected_state = if expected {
            MultipartUploadState::Completing
        } else {
            MultipartUploadState::InProgress
        };
        assert!(
            session.state == expected_state,
            "{row}: unexpected state {:?} (a rejected acquire must leave it untouched)",
            session.state
        );
        assert_eq!(
            session.lease_until.is_some(),
            expected,
            "{row}: lease_until is recorded on a successful acquire and never written on a rejected one"
        );
    }
}

/// `finish_complete` is fenced by the lease owner: a different owner (a takeover) loses its CAS
/// and leaves the session `completing` with no result and its lease untouched; the current lease
/// owner's own call completes the session and persists the snapshot.
#[tokio::test]
async fn finish_complete_is_fenced_by_lease_owner() {
    // (row, owner asserted by finish_complete, expected finished)
    for (row, owner, expected) in [
        ("foreign lease owner", "completer-b", false),
        ("matching lease owner", "completer-a", true),
    ] {
        let db = db().await;
        let conn = db.conn().expect("conn");
        let multipart = MultipartRepo::new();
        let now = OffsetDateTime::now_utc();
        let (_file_id, upload_id) =
            seed_session(&conn, &multipart, now + time::Duration::hours(1), now).await;
        assert!(
            acquire(&conn, &multipart, upload_id, "completer-a", now).await,
            "{row}: setup must acquire the lease before completing"
        );

        let finished = multipart
            .finish_complete(&conn, upload_id, Some(owner), "{\"snapshot\":true}")
            .await
            .expect("finish_complete must not error even when its CAS loses");
        assert_eq!(finished, expected, "{row}: finished");

        let session = multipart
            .get(&conn, upload_id)
            .await
            .expect("get must not error")
            .expect("session must still exist");
        if expected {
            assert!(
                matches!(session.state, MultipartUploadState::Completed),
                "{row}"
            );
            assert_eq!(
                session.complete_result.as_deref(),
                Some("{\"snapshot\":true}"),
                "{row}"
            );
        } else {
            assert!(
                matches!(session.state, MultipartUploadState::Completing),
                "{row}: a rejected finish_complete must leave state untouched"
            );
            assert!(
                session.complete_result.is_none(),
                "{row}: a rejected finish_complete must not persist a complete_result snapshot"
            );
            assert!(
                session.lease_until.is_some(),
                "{row}: a rejected finish_complete must not clear the (still legitimately held) lease"
            );
        }
    }
}

/// How `has_active_for_file_reflects_session_state` prepares the file's multipart session.
#[derive(Clone, Copy)]
enum Session {
    /// The file has no session at all.
    None,
    /// An `in_progress` session within its TTL.
    Live,
    /// A session driven to `completed` through the real lease + finish calls.
    Completed,
    /// `completing` under a live lease: a completer is assembling the object now.
    CompletingLive,
    /// `completing` with a lapsed lease: reaping it is the sweep's job, so it still counts.
    CompletingExpiredLease,
}

#[tokio::test]
async fn has_active_for_file_reflects_session_state() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::multipart_upload::{
        Column as UploadColumn, Entity as UploadEntity,
    };

    let cases = [
        ("no session", Session::None, false),
        ("live in_progress session", Session::Live, true),
        ("terminal (completed) session", Session::Completed, false),
        ("completing, live lease", Session::CompletingLive, true),
        (
            "completing, expired lease",
            Session::CompletingExpiredLease,
            true,
        ),
    ];
    for (name, session, expected) in cases {
        let db = db().await;
        let conn = db.conn().expect("conn");
        let multipart = MultipartRepo::new();
        let now = OffsetDateTime::now_utc();
        let expires_at = now + time::Duration::hours(1);

        let file_id = if let Session::None = session {
            let file_id = Uuid::now_v7();
            FileRepo::new()
                .create(
                    &conn,
                    &AccessScope::allow_all(),
                    &new_file(file_id, Uuid::now_v7()),
                )
                .await
                .expect("create parent file");
            file_id
        } else {
            let (file_id, upload_id) = seed_session(&conn, &multipart, expires_at, now).await;
            match session {
                Session::Completed | Session::CompletingLive => {
                    assert!(
                        acquire(&conn, &multipart, upload_id, "completer-a", now).await,
                        "{name}: setup must acquire the lease"
                    );
                    if let Session::Completed = session {
                        let finished = multipart
                            .finish_complete(&conn, upload_id, Some("completer-a"), "{}")
                            .await
                            .expect("finish_complete must not error");
                        assert!(finished, "{name}: setup must reach completed");
                    }
                }
                Session::CompletingExpiredLease => {
                    UploadEntity::update_many()
                        .col_expr(UploadColumn::State, Expr::value("completing"))
                        .col_expr(
                            UploadColumn::LeaseUntil,
                            Expr::value(Some(now - time::Duration::hours(1))),
                        )
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
                }
                Session::None | Session::Live => {}
            }
            file_id
        };

        let has_active = multipart
            .has_active_for_file(&conn, file_id)
            .await
            .expect("has_active_for_file must not error");
        assert_eq!(has_active, expected, "{name}");
    }
}
