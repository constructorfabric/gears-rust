//! Repo- and store-level predicates on a real `SQLite` DB with the full migration applied:
//! version lookups, manifests batching, policy upsert and multipart sessions (lease fencing).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use sea_orm::{EntityTrait, PaginatorTrait};
use time::OffsetDateTime;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::domain::multipart::MultipartUploadState;
use file_storage::domain::policy::{PolicyBody, PolicyScope, SizeLimits};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::entity::policy::Entity as PolicyEntity;
use file_storage::infra::storage::repo::{FileRepo, MultipartRepo, VersionRepo};
use file_storage_sdk::FileVersion;

mod common;

const TENANT: &str = "00000000-0000-0000-0000-0000000000a1";
const OWNER: &str = "00000000-0000-0000-0000-0000000000b1";

fn new_version(file_id: Uuid, version_id: Uuid, size: i64) -> FileVersion {
    FileVersion {
        size,
        ..common::version_row(file_id, version_id)
    }
}

async fn db() -> Arc<DBProvider<DbError>> {
    common::build_db("repo").await
}

/// `get(file_id, version_id)` resolves exactly the target row among many versions of two files,
/// and never under a foreign `file_id`.
#[tokio::test]
async fn version_repo_get_returns_correct_row_among_many() {
    let db = common::build_db("repo").await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let file_a = Uuid::now_v7();
    let file_b = Uuid::now_v7();
    let tenant = Uuid::now_v7();
    files
        .create(&conn, &scope, &common::file_row(file_a, tenant))
        .await
        .expect("create file_a");
    files
        .create(&conn, &scope, &common::file_row(file_b, tenant))
        .await
        .expect("create file_b");

    // The target lives in file_a; every other row must be excluded by `get`.
    let mut file_a_versions = Vec::new();
    for i in 0..5u8 {
        let vid = Uuid::now_v7();
        versions
            .insert(&conn, &scope, &new_version(file_a, vid, i64::from(i) * 10))
            .await
            .expect("insert file_a version");
        file_a_versions.push(vid);
    }
    for _ in 0..5u8 {
        versions
            .insert(&conn, &scope, &new_version(file_b, Uuid::now_v7(), 999))
            .await
            .expect("insert file_b version");
    }
    let target = file_a_versions[2];

    let found = versions
        .get(&conn, &scope, file_a, target)
        .await
        .expect("get must not error")
        .expect("target version must be found");
    assert_eq!(found.file_id, file_a);
    assert_eq!(found.version_id, target);
    assert_eq!(found.size, 20, "must be the i==2 row, not any other");

    let cross = versions
        .get(&conn, &scope, file_b, target)
        .await
        .expect("get must not error");
    assert!(
        cross.is_none(),
        "a version_id belonging to file_a must not resolve under file_b"
    );
}

/// `VersionRepo::get_manifests` returns every requested version's manifest even when the id list
/// spans more than one `max_bind_params_for` chunk (30 000 on `SQLite`, minus
/// `GET_MANIFESTS_RESERVED_PARAMS`). Only the versions at chunk edges exist; the rest of the list
/// is ids without a row, which are simply absent from the result.
#[tokio::test]
async fn get_manifests_returns_results_from_every_chunk() {
    const LIST_LEN: usize = 35_000;
    // The last id of the first chunk, the first of the second, and the very last.
    const SEEDED_AT: [usize; 4] = [0, 29_983, 29_984, LIST_LEN - 1];

    let db = common::build_db("repo").await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let (file_id, _) =
        common::seed_file_with_versions(&db, Uuid::now_v7(), Uuid::now_v7(), &[]).await;
    let now = OffsetDateTime::now_utc();

    let mut version_ids: Vec<Uuid> = (0..LIST_LEN).map(|_| Uuid::now_v7()).collect();
    for at in SEEDED_AT {
        version_ids[at] =
            common::insert_composite_version(&db, file_id, now, &format!("m{at}")).await;
    }

    let manifests = VersionRepo::new()
        .get_manifests(&conn, &scope, &version_ids)
        .await
        .expect("get_manifests must not error across chunk boundaries");

    assert_eq!(manifests.len(), SEEDED_AT.len(), "{manifests:?}");
    for at in SEEDED_AT {
        assert_eq!(
            manifests.get(&version_ids[at]),
            Some(&format!("m{at}")),
            "manifest at list position {at}"
        );
    }
}

fn body_with_max_bytes(max_bytes: u64) -> PolicyBody {
    PolicyBody {
        size_limits: SizeLimits {
            max_bytes: Some(max_bytes),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Two sequential upserts for one scope leave exactly one row carrying the second body, for a
/// tenant-scope row (no owner) and a user-scope row (`scope_owner_id = Some(..)`).
#[tokio::test]
async fn policy_upsert_on_conflict_updates_existing_row_not_duplicates() {
    let owner_id: Uuid = OWNER.parse().expect("valid uuid");
    for (policy_scope, scope_owner_id) in [
        (PolicyScope::Tenant, None),
        (PolicyScope::User, Some(owner_id)),
    ] {
        let (db, dsn) = common::build_db_with_dsn("repo").await;
        let store = Store::new(db);
        let scope = AccessScope::allow_all();
        let tenant_id: Uuid = TENANT.parse().expect("valid uuid");
        let now = time::OffsetDateTime::now_utc();

        let first_body = body_with_max_bytes(100);
        let second_body = body_with_max_bytes(200);

        for body in [&first_body, &second_body] {
            store
                .upsert_policy(&scope, tenant_id, &policy_scope, scope_owner_id, body, now)
                .await
                .expect("upsert must succeed");
        }

        // Independent raw connection, purely for the row count.
        let raw = sea_orm::Database::connect(&dsn)
            .await
            .expect("second raw connection");
        let count = PolicyEntity::find()
            .count(&raw)
            .await
            .expect("count policies");
        assert_eq!(
            count, 1,
            "two upserts for the same {policy_scope:?} scope must leave exactly one row, not {count}"
        );

        let stored = store
            .get_policy(&scope, tenant_id, &policy_scope, scope_owner_id)
            .await
            .expect("get_policy must succeed")
            .expect("policy row must exist");
        assert_eq!(
            stored.body, second_body,
            "the surviving row must carry the second call's body, not the first's"
        );
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
        .create(conn, &scope, &common::file_row(file_id, tenant_id))
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
                    &common::file_row(file_id, Uuid::now_v7()),
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
