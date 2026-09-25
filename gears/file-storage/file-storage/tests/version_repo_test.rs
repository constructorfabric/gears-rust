//! Repo-level test for `VersionRepo::get`'s direct-predicate rewrite (P2 2.2).
//!
//! Runs against a real SQLite DB with the full migration applied, exercising
//! `toolkit_db::secure`'s `DBRunner`/`AccessScope` machinery exactly as
//! `Store` does — a plain `sea_orm::Database::connect` cannot stand in here
//! because `VersionRepo::get`/`insert`/`list_by_file` require a `DBRunner`,
//! which is only obtainable via `DBProvider::conn()`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::{FileRepo, VersionRepo};
use file_storage_sdk::{File, FileVersion, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file SQLite DB (mirrors `service_test.rs::build_service`) —
/// a bare `sqlite::memory:` gives each pooled connection its own empty DB.
async fn db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-version-repo-test-{}.db",
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
        name: "doc.txt".to_owned(),
        gts_file_type: GTS.to_owned(),
        content_id: None,
        meta_version: 0,
        created_at: now,
        last_modified_at: now,
    }
}

fn new_version(file_id: Uuid, version_id: Uuid, size: i64) -> FileVersion {
    let now = OffsetDateTime::now_utc();
    FileVersion {
        file_id,
        version_id,
        mime_type: "text/plain".to_owned(),
        size,
        hash_algorithm: "SHA-256".to_owned(),
        hash_value: vec![0u8; 32],
        hash_mode: "whole-sha256".to_owned(),
        part_count: None,
        status: VersionStatus::Available,
        is_current: false,
        backend_id: "mem".to_owned(),
        backend_path: format!("/{file_id}/{version_id}"),
        created_at: now,
        bound_on_finalize: false,
    }
}

/// A `pending` version row with an explicit `created_at`, for the
/// `list_pending_older_than` batch-cap test below (needs precise control
/// over ordering, unlike [`new_version`]'s always-`now` `Available` row).
fn new_pending_version(file_id: Uuid, version_id: Uuid, created_at: OffsetDateTime) -> FileVersion {
    FileVersion {
        file_id,
        version_id,
        mime_type: "text/plain".to_owned(),
        size: 0,
        hash_algorithm: "SHA-256".to_owned(),
        hash_value: vec![0u8; 32],
        hash_mode: "whole-sha256".to_owned(),
        part_count: None,
        status: VersionStatus::Pending,
        is_current: false,
        backend_id: "mem".to_owned(),
        backend_path: format!("/{file_id}/{version_id}"),
        created_at,
        bound_on_finalize: false,
    }
}

/// `VersionRepo::get(file_id, version_id)` must resolve exactly the target
/// row among many versions seeded across two different files, and must never
/// resolve a version under a `file_id` it does not belong to.
///
/// This exercises the P2 2.2 rewrite of `get` from a `list_by_file` +
/// Rust-side `.find()` scan to a direct two-column SQL predicate: the old
/// code's comment claimed the direct predicate "proved unreliable across the
/// secure layer", but this test — plus `cargo clippy`/`cargo test` staying
/// green — did not reproduce that; the direct query resolves correctly.
#[tokio::test]
async fn version_repo_get_returns_correct_row_among_many() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let file_a = Uuid::now_v7();
    let file_b = Uuid::now_v7();
    let tenant = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_a, tenant))
        .await
        .expect("create file_a");
    files
        .create(&conn, &scope, &new_file(file_b, tenant))
        .await
        .expect("create file_b");

    // Seed several versions per file. The target lives in file_a; every
    // other row (in file_a and file_b) must be excluded by `get`.
    let mut target: Option<Uuid> = None;
    for i in 0..5u8 {
        let vid = Uuid::now_v7();
        versions
            .insert(&conn, &scope, &new_version(file_a, vid, i64::from(i) * 10))
            .await
            .expect("insert file_a version");
        if i == 2 {
            target = Some(vid);
        }
    }
    for _ in 0..5u8 {
        let vid = Uuid::now_v7();
        versions
            .insert(&conn, &scope, &new_version(file_b, vid, 999))
            .await
            .expect("insert file_b version");
    }
    let target = target.expect("target version seeded");

    let found = versions
        .get(&conn, &scope, file_a, target)
        .await
        .expect("get must not error")
        .expect("target version must be found");
    assert_eq!(found.file_id, file_a);
    assert_eq!(found.version_id, target);
    assert_eq!(found.size, 20, "must be the i==2 row, not any other");

    // Cross-file bleed check: the same version_id does not exist under
    // file_b, so looking it up scoped to file_b must resolve to nothing.
    let cross = versions
        .get(&conn, &scope, file_b, target)
        .await
        .expect("get must not error");
    assert!(
        cross.is_none(),
        "a version_id belonging to file_a must not resolve under file_b"
    );
}

/// `VersionRepo::get_manifests` must return every requested version's
/// manifest even when the id list spans more than one `max_bind_params_for`
/// chunk (30 000 on `SQLite`, minus `GET_MANIFESTS_RESERVED_PARAMS`).
///
/// `35_000` is chosen deliberately above `SQLite`'s own real bind-parameter
/// ceiling (32766), not just this repo's conservative 30 000 chunk budget --
/// before chunking, a single `IN (...)` query over this many ids fails
/// outright with a driver error instead of merely being slow, so this test
/// fails hard (not just "returns fewer rows than expected") on the
/// unchunked code.
///
/// Manifest rows -- plus their parent `files`/`file_versions` rows, `sqlx`'s
/// `SQLite` pool enables `PRAGMA foreign_keys = ON` by default -- are seeded
/// directly via `secure_insert_many` (bypassing `FileRepo::create`/
/// `VersionRepo::insert`/`insert_manifest`'s one-row-at-a-time APIs) so
/// setup itself stays fast.
#[tokio::test]
async fn get_manifests_returns_all_results_across_multiple_chunks() {
    use file_storage::infra::storage::entity::file::{
        ActiveModel as FileActiveModel, Entity as FileEntity,
    };
    use file_storage::infra::storage::entity::file_version::{
        ActiveModel as FileVersionActiveModel, Entity as FileVersionEntity,
    };
    use file_storage::infra::storage::entity::version_hash_manifest::{
        ActiveModel as ManifestActiveModel, Entity as ManifestEntity,
    };
    use sea_orm::Set;
    use toolkit_db::secure::secure_insert_many;

    const N: usize = 35_000;

    let db = db().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let versions = VersionRepo::new();
    let now = OffsetDateTime::now_utc();
    let tenant_id = Uuid::now_v7();

    let version_ids: Vec<Uuid> = (0..N).map(|_| Uuid::now_v7()).collect();
    let file_ids: Vec<Uuid> = (0..N).map(|_| Uuid::now_v7()).collect();

    let file_models: Vec<FileActiveModel> = file_ids
        .iter()
        .map(|&file_id| FileActiveModel {
            file_id: Set(file_id),
            tenant_id: Set(tenant_id),
            owner_kind: Set("user".to_owned()),
            owner_id: Set(Uuid::now_v7()),
            name: Set("f.bin".to_owned()),
            gts_file_type: Set(GTS.to_owned()),
            content_id: Set(None),
            meta_version: Set(0),
            created_at: Set(now),
            last_modified_at: Set(now),
        })
        .collect();
    secure_insert_many::<FileEntity>(file_models, &scope, &conn)
        .await
        .expect("seed parent files rows");

    let version_models: Vec<FileVersionActiveModel> = file_ids
        .iter()
        .zip(version_ids.iter())
        .map(|(&file_id, &version_id)| FileVersionActiveModel {
            file_id: Set(file_id),
            version_id: Set(version_id),
            mime_type: Set("application/octet-stream".to_owned()),
            size: Set(1024),
            hash_algorithm: Set("SHA-256".to_owned()),
            hash_value: Set(vec![0u8; 32]),
            hash_mode: Set("multipart-composite-sha256".to_owned()),
            part_count: Set(Some(2)),
            status: Set("available".to_owned()),
            is_current: Set(false),
            backend_id: Set("mem".to_owned()),
            backend_path: Set(format!("/{file_id}/{version_id}")),
            created_at: Set(now),
            bound_on_finalize: Set(false),
        })
        .collect();
    secure_insert_many::<FileVersionEntity>(version_models, &scope, &conn)
        .await
        .expect("seed parent file_versions rows");

    let models: Vec<ManifestActiveModel> = version_ids
        .iter()
        .map(|&version_id| ManifestActiveModel {
            version_id: Set(version_id),
            manifest: Set(format!("{{\"version_id\":\"{version_id}\"}}")),
            created_at: Set(now),
        })
        .collect();
    secure_insert_many::<ManifestEntity>(models, &scope, &conn)
        .await
        .expect("seed manifest rows");

    let manifests = versions
        .get_manifests(&conn, &scope, &version_ids)
        .await
        .expect("get_manifests must not error across chunk boundaries");

    assert_eq!(
        manifests.len(),
        N,
        "every seeded version_id must have a manifest entry back, regardless \
         of how many chunks the id list was split into"
    );
    for &version_id in &version_ids {
        assert_eq!(
            manifests.get(&version_id),
            Some(&format!("{{\"version_id\":\"{version_id}\"}}")),
            "manifest content must round-trip for {version_id}"
        );
    }
}

/// `VersionRepo::list_by_file` must order `(created_at, version_id)`
/// descending, not `created_at` alone -- otherwise two versions of
/// the same file sharing a `created_at` instant have no defined relative
/// order across two separate `OFFSET` queries, and an `OFFSET`-paginated page
/// boundary drawn through such a run can skip or repeat a row.
///
/// Seeds four versions of one file, all sharing the SAME `created_at`
/// (`version_id`s built via `Uuid::from_u128`, not `Uuid::now_v7`, so the
/// tiebreak assertion does not depend on wall-clock generation order), then
/// walks `limit = 2` pages via `offset` and asserts the concatenated pages
/// are exactly the four `version_id`s, each exactly once, in
/// `version_id`-descending order.
#[tokio::test]
async fn list_by_file_orders_by_created_at_then_version_id_so_paged_offsets_do_not_skip_or_repeat()
{
    let db = db().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let file_id = Uuid::now_v7();
    let tenant = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant))
        .await
        .expect("create file");

    let same_instant = OffsetDateTime::now_utc();
    let ids = [
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        Uuid::from_u128(3),
        Uuid::from_u128(4),
    ];
    for &version_id in &ids {
        versions
            .insert(
                &conn,
                &scope,
                &new_pending_version(file_id, version_id, same_instant),
            )
            .await
            .expect("insert version");
    }

    let page1 = versions
        .list_by_file(&conn, &scope, file_id, 2, 0)
        .await
        .expect("page 1");
    let page2 = versions
        .list_by_file(&conn, &scope, file_id, 2, 2)
        .await
        .expect("page 2");

    let mut seen: Vec<Uuid> = page1
        .iter()
        .chain(page2.iter())
        .map(|v| v.version_id)
        .collect();
    let mut expected = ids.to_vec();
    expected.sort_unstable_by(|a, b| b.cmp(a)); // version_id descending
    assert_eq!(
        seen, expected,
        "two OFFSET pages over four equal-created_at rows must together cover \
         every version_id exactly once, in version_id-descending order"
    );
    seen.dedup();
    assert_eq!(
        seen.len(),
        4,
        "no version_id may be repeated across the two pages"
    );
}

/// `VersionRepo::list_pending_older_than` must cap its result at `limit` rows
/// -- the cleanup sweep's abandoned-pending-version phase used to run this
/// query with no bound at all, materializing an entire backlog in one sweep
/// pass -- and must order deterministically: `(created_at, version_id)`
/// ascending, not merely `created_at` (which alone leaves ties unordered).
///
/// Seeds four pending versions under one file, one more than `limit = 3`:
/// `a` strictly oldest, `d` strictly newest (must be excluded), and `b`/`c`
/// sharing `a`'s successor timestamp with `b`'s `version_id` numerically
/// below `c`'s (constructed via `Uuid::from_u128`, not `Uuid::now_v7`, so the
/// tiebreak assertion does not depend on wall-clock generation order).
#[tokio::test]
async fn list_pending_older_than_caps_at_limit_and_orders_deterministically() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let file_id = Uuid::now_v7();
    let tenant = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant))
        .await
        .expect("create file");

    let base = OffsetDateTime::now_utc() - time::Duration::hours(10);
    let id_a = Uuid::from_u128(1);
    let id_b = Uuid::from_u128(11);
    let id_c = Uuid::from_u128(12);
    let id_d = Uuid::from_u128(99);

    versions
        .insert(&conn, &scope, &new_pending_version(file_id, id_a, base))
        .await
        .expect("insert a");
    versions
        .insert(
            &conn,
            &scope,
            &new_pending_version(file_id, id_c, base + time::Duration::seconds(1)),
        )
        .await
        .expect("insert c");
    versions
        .insert(
            &conn,
            &scope,
            &new_pending_version(file_id, id_b, base + time::Duration::seconds(1)),
        )
        .await
        .expect("insert b");
    versions
        .insert(
            &conn,
            &scope,
            &new_pending_version(file_id, id_d, base + time::Duration::seconds(2)),
        )
        .await
        .expect("insert d (newest, must be excluded by the limit)");

    let now = OffsetDateTime::now_utc();
    let older_than = now;
    let rows = versions
        .list_pending_older_than(&conn, &scope, older_than, now, 3)
        .await
        .expect("list_pending_older_than must not error");

    assert_eq!(
        rows.len(),
        3,
        "limit = 3 over 4 eligible candidates must return exactly 3 rows"
    );
    assert_eq!(
        rows.iter().map(|r| r.version_id).collect::<Vec<_>>(),
        vec![id_a, id_b, id_c],
        "rows must be ordered (created_at, version_id) ascending -- a first \
         (oldest), then b before c (same created_at, b's version_id is \
         smaller), and d (strictly newest) excluded by the limit"
    );
}

/// `VersionRepo::insert` against a `file_id` whose `files` row is already
/// gone must surface `DomainError::FileNotFound` (HTTP 404), not the
/// untyped `Database` shape (HTTP 500) a bare `db_err`-mapped FK violation
/// would have produced.
///
/// This is the deleted-parent half of the delete-vs-insert-version race
/// (`docs/concurrency-and-failure-model.md` race #9/#10): on real
/// `PostgreSQL`, `FileRepo::lock_for_update` makes a losing
/// `insert_pending_version` block until the delete's transaction commits,
/// then fail exactly this FK check. `SQLite` needs no lock to reproduce the
/// FK failure itself (`sqlx`'s `PRAGMA foreign_keys = ON` enforces it
/// unconditionally) -- this test pins the error-mapping half of the fix
/// deterministically, without needing real concurrency or `PostgreSQL`; see
/// `tests/pg_concurrency_test.rs` for the concurrency half.
#[tokio::test]
async fn insert_against_a_deleted_file_maps_foreign_key_violation_to_file_not_found() {
    let db = db().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let file_id = Uuid::now_v7();
    let tenant = Uuid::now_v7();
    files
        .create(&conn, &scope, &new_file(file_id, tenant))
        .await
        .expect("create file");

    let removed = files
        .delete(&conn, &scope, file_id)
        .await
        .expect("delete must not error");
    assert!(removed, "the file row must have been removed");

    let version_id = Uuid::now_v7();
    let err = versions
        .insert(&conn, &scope, &new_version(file_id, version_id, 0))
        .await
        .expect_err("insert against a deleted file's file_id must fail");

    match err {
        file_storage::domain::error::DomainError::FileNotFound { id } => {
            assert_eq!(
                id, file_id,
                "the FileNotFound error must name the file_id the FK check failed against"
            );
        }
        other => panic!(
            "expected DomainError::FileNotFound (mapped from the foreign-key \
             violation), got: {other}"
        ),
    }
}
