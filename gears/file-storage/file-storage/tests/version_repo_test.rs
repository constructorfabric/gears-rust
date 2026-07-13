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
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let dsn = format!("sqlite://{file}?mode=rwc");
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
