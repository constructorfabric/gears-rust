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
