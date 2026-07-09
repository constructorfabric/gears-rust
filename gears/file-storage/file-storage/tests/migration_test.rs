//! Schema-level tests for the P1 initial migration, run against a real
//! in-memory SQLite database (~1ms per DB). These verify that the SQL itself is
//! correct — every `CHECK` constraint, the partial unique "current version"
//! index, composite primary keys, and `ON DELETE CASCADE` — without needing a
//! running server. PostgreSQL-dialect behaviour (domain types, schema
//! namespace, FK RESTRICT) is covered by E2E tests, not here.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;

use file_storage::Migrator;

const TENANT: &str = "00000000-0000-0000-0000-0000000000a1";
const OWNER: &str = "00000000-0000-0000-0000-0000000000b1";
const FILE: &str = "00000000-0000-0000-0000-0000000000c1";
const GTS: &str = "gts.cf.fstorage.file.type.v1~x.test.v1~";
/// 32 zero bytes — the only hash length the P1 `SHA-256` CHECK accepts.
const HASH32: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn stmt(db: &DatabaseConnection, sql: impl Into<String>) -> Statement {
    Statement::from_string(db.get_database_backend(), sql.into())
}

/// Fresh in-memory SQLite with the P1 migration applied and FK enforcement on
/// (SQLite leaves foreign keys off by default, so cascade would silently no-op).
async fn migrated_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    db.execute(stmt(&db, "PRAGMA foreign_keys = ON;"))
        .await
        .expect("enable foreign keys");
    Migrator::up(&db, None).await.expect("apply P1 migration");
    db
}

async fn insert_file(db: &DatabaseConnection, file_id: &str) {
    db.execute(stmt(
        db,
        format!(
            "INSERT INTO files (file_id, tenant_id, owner_kind, owner_id, name, gts_file_type) \
             VALUES ('{file_id}', '{TENANT}', 'user', '{OWNER}', 'doc.txt', '{GTS}')"
        ),
    ))
    .await
    .expect("insert file");
}

async fn insert_version(db: &DatabaseConnection, file_id: &str, version_id: &str, is_current: u8) {
    db.execute(stmt(
        db,
        format!(
            "INSERT INTO file_versions \
             (file_id, version_id, mime_type, size, hash_value, status, is_current, backend_id, backend_path) \
             VALUES ('{file_id}', '{version_id}', 'text/plain', 0, X'{HASH32}', 'available', {is_current}, 'local', '/{file_id}/{version_id}')"
        ),
    ))
    .await
    .expect("insert version");
}

async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(stmt(db, sql))
        .await
        .expect("count query")
        .expect("one row")
        .try_get::<i64>("", "c")
        .expect("i64 column c")
}

// ── schema existence / lifecycle ─────────────────────────────────────────────

#[tokio::test]
async fn migration_creates_all_three_tables() {
    let db = migrated_db().await;
    for table in ["files", "file_versions", "files_custom_metadata"] {
        let probe = db
            .execute(stmt(&db, format!("SELECT * FROM {table} LIMIT 0")))
            .await;
        assert!(
            probe.is_ok(),
            "table {table} must exist after up: {probe:?}"
        );
    }
}

#[tokio::test]
async fn migration_up_down_up_roundtrip() {
    let db = migrated_db().await;

    Migrator::down(&db, None).await.expect("roll back");
    let gone = db.execute(stmt(&db, "SELECT * FROM files LIMIT 0")).await;
    assert!(gone.is_err(), "files must be dropped by down(): {gone:?}");

    Migrator::up(&db, None).await.expect("re-apply");
    let back = db.execute(stmt(&db, "SELECT * FROM files LIMIT 0")).await;
    assert!(back.is_ok(), "files must exist again after re-up: {back:?}");
}

// ── files CHECK constraints ──────────────────────────────────────────────────

#[tokio::test]
async fn files_accepts_user_and_app_owner_kinds() {
    let db = migrated_db().await;
    db.execute(stmt(
        &db,
        format!(
            "INSERT INTO files (file_id, tenant_id, owner_kind, owner_id, name, gts_file_type) \
             VALUES ('{FILE}', '{TENANT}', 'user', '{OWNER}', 'a', '{GTS}'), \
                    ('00000000-0000-0000-0000-0000000000c2', '{TENANT}', 'app', '{OWNER}', 'b', '{GTS}')"
        ),
    ))
    .await
    .expect("both owner kinds are valid");
    assert_eq!(count(&db, "SELECT COUNT(*) AS c FROM files").await, 2);
}

#[tokio::test]
async fn files_rejects_invalid_owner_kind() {
    let db = migrated_db().await;
    let res = db
        .execute(stmt(
            &db,
            format!(
                "INSERT INTO files (file_id, tenant_id, owner_kind, owner_id, name, gts_file_type) \
                 VALUES ('{FILE}', '{TENANT}', 'robot', '{OWNER}', 'a', '{GTS}')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "owner_kind CHECK must reject 'robot': {res:?}"
    );
}

#[tokio::test]
async fn files_rejects_negative_meta_version() {
    let db = migrated_db().await;
    let res = db
        .execute(stmt(
            &db,
            format!(
                "INSERT INTO files (file_id, tenant_id, owner_kind, owner_id, name, gts_file_type, meta_version) \
                 VALUES ('{FILE}', '{TENANT}', 'user', '{OWNER}', 'a', '{GTS}', -1)"
            ),
        ))
        .await;
    assert!(res.is_err(), "meta_version CHECK must reject -1: {res:?}");
}

#[tokio::test]
async fn files_content_id_is_nullable_until_first_bind() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await; // no content_id supplied
    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT COUNT(*) AS c FROM files WHERE file_id = '{FILE}' AND content_id IS NULL"
            )
        )
        .await,
        1,
        "content_id must default to NULL"
    );
}

// ── file_versions CHECK constraints ──────────────────────────────────────────

#[tokio::test]
async fn file_versions_accepts_valid_row() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_version(&db, FILE, "00000000-0000-0000-0000-0000000000d1", 1).await;
    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM file_versions").await,
        1
    );
}

#[tokio::test]
async fn file_versions_rejects_negative_size() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute(stmt(
            &db,
            format!(
                "INSERT INTO file_versions (file_id, version_id, mime_type, size, hash_value, backend_id, backend_path) \
                 VALUES ('{FILE}', '00000000-0000-0000-0000-0000000000d1', 'text/plain', -1, X'{HASH32}', 'local', '/p')"
            ),
        ))
        .await;
    assert!(res.is_err(), "size CHECK must reject -1: {res:?}");
}

#[tokio::test]
async fn file_versions_rejects_unknown_status() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute(stmt(
            &db,
            format!(
                "INSERT INTO file_versions (file_id, version_id, mime_type, size, hash_value, status, backend_id, backend_path) \
                 VALUES ('{FILE}', '00000000-0000-0000-0000-0000000000d1', 'text/plain', 0, X'{HASH32}', 'frozen', 'local', '/p')"
            ),
        ))
        .await;
    assert!(res.is_err(), "status CHECK must reject 'frozen': {res:?}");
}

#[tokio::test]
async fn file_versions_rejects_non_sha256_algorithm_in_p1() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    // BLAKE3 is only widened in by the P2 migration; P1 is locked to SHA-256.
    let res = db
        .execute(stmt(
            &db,
            format!(
                "INSERT INTO file_versions (file_id, version_id, mime_type, size, hash_algorithm, hash_value, backend_id, backend_path) \
                 VALUES ('{FILE}', '00000000-0000-0000-0000-0000000000d1', 'text/plain', 0, 'BLAKE3', X'{HASH32}', 'local', '/p')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "hash_algorithm CHECK must reject BLAKE3 in P1: {res:?}"
    );
}

#[tokio::test]
async fn file_versions_rejects_wrong_hash_length() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute(stmt(
            &db,
            format!(
                "INSERT INTO file_versions (file_id, version_id, mime_type, size, hash_value, backend_id, backend_path) \
                 VALUES ('{FILE}', '00000000-0000-0000-0000-0000000000d1', 'text/plain', 0, X'00112233', 'local', '/p')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "hash_value length CHECK must reject 4 bytes: {res:?}"
    );
}

// ── partial unique index: at most one current version per file ───────────────

#[tokio::test]
async fn file_versions_allows_only_one_current_per_file() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_version(&db, FILE, "00000000-0000-0000-0000-0000000000d1", 1).await;
    let res = db
        .execute(stmt(
            &db,
            format!(
                "INSERT INTO file_versions (file_id, version_id, mime_type, size, hash_value, is_current, backend_id, backend_path) \
                 VALUES ('{FILE}', '00000000-0000-0000-0000-0000000000d2', 'text/plain', 0, X'{HASH32}', 1, 'local', '/p')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "two current versions for one file must violate the unique index: {res:?}"
    );
}

#[tokio::test]
async fn file_versions_allows_many_non_current_per_file() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_version(&db, FILE, "00000000-0000-0000-0000-0000000000d1", 0).await;
    insert_version(&db, FILE, "00000000-0000-0000-0000-0000000000d2", 0).await;
    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM file_versions").await,
        2,
        "multiple non-current versions are allowed"
    );
}

#[tokio::test]
async fn file_versions_allows_current_per_distinct_file() {
    let db = migrated_db().await;
    let file2 = "00000000-0000-0000-0000-0000000000c2";
    insert_file(&db, FILE).await;
    insert_file(&db, file2).await;
    insert_version(&db, FILE, "00000000-0000-0000-0000-0000000000d1", 1).await;
    insert_version(&db, file2, "00000000-0000-0000-0000-0000000000d2", 1).await;
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) AS c FROM file_versions WHERE is_current = 1"
        )
        .await,
        2
    );
}

// ── custom metadata composite PK ─────────────────────────────────────────────

#[tokio::test]
async fn custom_metadata_rejects_duplicate_key_per_file() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    db.execute(stmt(
        &db,
        format!(
            "INSERT INTO files_custom_metadata (file_id, key, value) VALUES ('{FILE}', 'tag', 'a')"
        ),
    ))
    .await
    .expect("first key insert");
    let res = db
        .execute(stmt(
            &db,
            format!("INSERT INTO files_custom_metadata (file_id, key, value) VALUES ('{FILE}', 'tag', 'b')"),
        ))
        .await;
    assert!(
        res.is_err(),
        "(file_id, key) PK must reject duplicate key: {res:?}"
    );
}

// ── cascade delete (FK enforcement enabled) ──────────────────────────────────

#[tokio::test]
async fn deleting_file_cascades_to_versions_and_metadata() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_version(&db, FILE, "00000000-0000-0000-0000-0000000000d1", 1).await;
    db.execute(stmt(
        &db,
        format!(
            "INSERT INTO files_custom_metadata (file_id, key, value) VALUES ('{FILE}', 'tag', 'a')"
        ),
    ))
    .await
    .expect("insert metadata");

    db.execute(stmt(
        &db,
        format!("DELETE FROM files WHERE file_id = '{FILE}'"),
    ))
    .await
    .expect("delete file");

    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM file_versions").await,
        0,
        "versions must be cascade-deleted with the file"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM files_custom_metadata").await,
        0,
        "custom metadata must be cascade-deleted with the file"
    );
}
