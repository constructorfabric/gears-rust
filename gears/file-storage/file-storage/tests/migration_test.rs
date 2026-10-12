#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use toolkit_gts::gts_id;

use file_storage::Migrator;

const TENANT: &str = "00000000-0000-0000-0000-0000000000a1";
const OWNER: &str = "00000000-0000-0000-0000-0000000000b1";
const FILE: &str = "00000000-0000-0000-0000-0000000000c1";
const VERSION: &str = "00000000-0000-0000-0000-0000000000d1";
const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");
const HASH32: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn stmt(db: &DatabaseConnection, sql: impl Into<String>) -> Statement {
    Statement::from_string(db.get_database_backend(), sql.into())
}

/// Fresh in-memory SQLite with migrations applied and FK enforcement on (SQLite defaults to off,
/// so cascades would silently no-op).
async fn migrated_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    db.execute_raw(stmt(&db, "PRAGMA foreign_keys = ON;"))
        .await
        .expect("enable foreign keys");
    Migrator::up(&db, None).await.expect("apply P1 migration");
    db
}

async fn insert_file(db: &DatabaseConnection, file_id: &str) {
    db.execute_raw(stmt(
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
    db.execute_raw(stmt(
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
    db.query_one_raw(stmt(db, sql))
        .await
        .expect("count query")
        .expect("one row")
        .try_get::<i64>("", "c")
        .expect("i64 column c")
}

#[tokio::test]
async fn migration_creates_all_three_tables() {
    let db = migrated_db().await;
    for table in ["files", "file_versions", "files_custom_metadata"] {
        let probe = db
            .execute_raw(stmt(&db, format!("SELECT * FROM {table} LIMIT 0")))
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
    let gone = db
        .execute_raw(stmt(&db, "SELECT * FROM files LIMIT 0"))
        .await;
    assert!(gone.is_err(), "files must be dropped by down(): {gone:?}");

    Migrator::up(&db, None).await.expect("re-apply");
    let back = db
        .execute_raw(stmt(&db, "SELECT * FROM files LIMIT 0"))
        .await;
    assert!(back.is_ok(), "files must exist again after re-up: {back:?}");
}

#[tokio::test]
async fn files_accepts_user_and_app_owner_kinds() {
    let db = migrated_db().await;
    db.execute_raw(stmt(
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
        .execute_raw(stmt(
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
        .execute_raw(stmt(
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
        .execute_raw(stmt(
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
        .execute_raw(stmt(
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
    let res = db
        .execute_raw(stmt(
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
        .execute_raw(stmt(
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

#[tokio::test]
async fn file_versions_allows_only_one_current_per_file() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_version(&db, FILE, "00000000-0000-0000-0000-0000000000d1", 1).await;
    let res = db
        .execute_raw(stmt(
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

#[tokio::test]
async fn custom_metadata_rejects_duplicate_key_per_file() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO files_custom_metadata (file_id, key, value) VALUES ('{FILE}', 'tag', 'a')"
        ),
    ))
    .await
    .expect("first key insert");
    let res = db
        .execute_raw(stmt(
            &db,
            format!("INSERT INTO files_custom_metadata (file_id, key, value) VALUES ('{FILE}', 'tag', 'b')"),
        ))
        .await;
    assert!(
        res.is_err(),
        "(file_id, key) PK must reject duplicate key: {res:?}"
    );
}

/// `request_hash` is `NOT NULL DEFAULT` empty blob: an INSERT omitting it succeeds, and such a row
/// fails closed on any replay.
#[tokio::test]
async fn idempotency_keys_request_hash_column_exists_with_default() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO idempotency_keys \
             (tenant_id, owner_kind, owner_id, idempotency_key, file_id, \
              response_status, response_body, response_etag, expires_at) \
             VALUES ('{TENANT}', 'user', '{OWNER}', 'k1', '{FILE}', \
                     201, '{{}}', 'etag', '2999-01-01T00:00:00Z')"
        ),
    ))
    .await
    .expect("insert idempotency row omitting request_hash must succeed");

    let hash_len = db
        .query_one_raw(stmt(
            &db,
            format!(
                "SELECT LENGTH(request_hash) AS c FROM idempotency_keys \
                 WHERE tenant_id = '{TENANT}' AND idempotency_key = 'k1'"
            ),
        ))
        .await
        .expect("select request_hash length")
        .expect("one row")
        .try_get::<i64>("", "c")
        .expect("i64 column c");
    assert_eq!(
        hash_len, 0,
        "request_hash must default to an empty blob, not a populated/garbage value"
    );
}

/// `policies` ids are `TEXT` in the SQLite DDL, so UUIDs are plain quoted literals.
#[tokio::test]
async fn policies_unique_index_rejects_duplicate_scope_tuple() {
    let db = migrated_db().await;
    let owner2 = "00000000-0000-0000-0000-0000000000b2";
    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body) \
             VALUES ('00000000-0000-0000-0000-0000000000e1', '{TENANT}', 'user', '{owner2}', '{{}}')"
        ),
    ))
    .await
    .expect("first user-scope policy insert");

    let res = db
        .execute_raw(stmt(
            &db,
            format!(
                "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body) \
                 VALUES ('00000000-0000-0000-0000-0000000000e2', '{TENANT}', 'user', '{owner2}', '{{}}')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "duplicate (tenant_id, 'user', scope_owner_id) must violate \
         policies_user_scope_unique_idx: {res:?}"
    );
}

/// A plain `UNIQUE` would not catch this (NULLs are distinct); hence the partial index on
/// `scope_owner_id IS NULL`.
#[tokio::test]
async fn policies_unique_index_rejects_duplicate_tenant_scope() {
    let db = migrated_db().await;
    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body) \
             VALUES ('00000000-0000-0000-0000-0000000000e3', '{TENANT}', 'tenant', NULL, '{{}}')"
        ),
    ))
    .await
    .expect("first tenant-scope policy insert");

    let res = db
        .execute_raw(stmt(
            &db,
            format!(
                "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body) \
                 VALUES ('00000000-0000-0000-0000-0000000000e4', '{TENANT}', 'tenant', NULL, '{{}}')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "duplicate (tenant_id, 'tenant') with NULL scope_owner_id must \
         violate policies_tenant_scope_unique_idx: {res:?}"
    );
}

#[tokio::test]
async fn policies_unique_index_allows_distinct_scopes() {
    let db = migrated_db().await;
    let tenant2 = "00000000-0000-0000-0000-0000000000a2";
    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body) VALUES \
             ('00000000-0000-0000-0000-0000000000e5', '{TENANT}', 'tenant', NULL, '{{}}'), \
             ('00000000-0000-0000-0000-0000000000e6', '{TENANT}', 'user', '{OWNER}', '{{}}'), \
             ('00000000-0000-0000-0000-0000000000e7', '{tenant2}', 'user', '{OWNER}', '{{}}')"
        ),
    ))
    .await
    .expect("distinct scopes must not collide across the partial indexes");
    assert_eq!(count(&db, "SELECT COUNT(*) AS c FROM policies").await, 3);
}

/// Applies all but the last migration and seeds duplicates (as the old upsert race could leave),
/// then applies the last: it must dedup to the newest row before creating the unique indexes.
#[tokio::test]
async fn policies_unique_migration_dedups_preexisting_duplicates() {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");

    Migrator::up(&db, Some(5))
        .await
        .expect("apply migrations up to (not including) policies_unique_scope");

    let owner = "00000000-0000-0000-0000-0000000000b2";
    let older = "00000000-0000-0000-0000-0000000000e1";
    let newer = "00000000-0000-0000-0000-0000000000e2";

    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body, updated_at) \
             VALUES ('{older}', '{TENANT}', 'user', '{owner}', '{{\"v\":1}}', '2026-01-01T00:00:00Z')"
        ),
    ))
    .await
    .expect("insert older duplicate user-scope policy");
    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body, updated_at) \
             VALUES ('{newer}', '{TENANT}', 'user', '{owner}', '{{\"v\":2}}', '2026-06-01T00:00:00Z')"
        ),
    ))
    .await
    .expect("insert newer duplicate user-scope policy");

    let tenant_older = "00000000-0000-0000-0000-0000000000e3";
    let tenant_newer = "00000000-0000-0000-0000-0000000000e4";
    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body, updated_at) \
             VALUES ('{tenant_older}', '{TENANT}', 'tenant', NULL, '{{\"v\":1}}', '2026-01-01T00:00:00Z')"
        ),
    ))
    .await
    .expect("insert older duplicate tenant-scope policy");
    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body, updated_at) \
             VALUES ('{tenant_newer}', '{TENANT}', 'tenant', NULL, '{{\"v\":2}}', '2026-06-01T00:00:00Z')"
        ),
    ))
    .await
    .expect("insert newer duplicate tenant-scope policy");

    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM policies").await,
        4,
        "all four duplicate rows must be present before the dedup migration runs"
    );

    Migrator::up(&db, Some(1))
        .await
        .expect("policies_unique_scope migration must dedup before creating the unique indexes");

    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT COUNT(*) AS c FROM policies WHERE tenant_id = '{TENANT}' AND scope = 'user' AND scope_owner_id = '{owner}'"
            )
        )
        .await,
        1,
        "duplicate user-scope rows must be deduped to exactly one"
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT COUNT(*) AS c FROM policies WHERE policy_id = '{newer}'")
        )
        .await,
        1,
        "the surviving user-scope row must be the most-recently-updated one"
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT COUNT(*) AS c FROM policies WHERE policy_id = '{older}'")
        )
        .await,
        0,
        "the stale user-scope duplicate must have been deleted"
    );

    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT COUNT(*) AS c FROM policies WHERE tenant_id = '{TENANT}' AND scope = 'tenant' AND scope_owner_id IS NULL"
            )
        )
        .await,
        1,
        "duplicate tenant-scope rows must be deduped to exactly one"
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT COUNT(*) AS c FROM policies WHERE policy_id = '{tenant_newer}'")
        )
        .await,
        1,
        "the surviving tenant-scope row must be the most-recently-updated one"
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT COUNT(*) AS c FROM policies WHERE policy_id = '{tenant_older}'")
        )
        .await,
        0,
        "the stale tenant-scope duplicate must have been deleted"
    );

    let dup_res = db
        .execute_raw(stmt(
            &db,
            format!(
                "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body) \
                 VALUES ('00000000-0000-0000-0000-0000000000e9', '{TENANT}', 'user', '{owner}', '{{}}')"
            ),
        ))
        .await;
    assert!(
        dup_res.is_err(),
        "policies_user_scope_unique_idx must reject a fresh duplicate after the dedup migration: {dup_res:?}"
    );
}

#[tokio::test]
async fn deleting_file_cascades_to_versions_and_metadata() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_version(&db, FILE, "00000000-0000-0000-0000-0000000000d1", 1).await;
    db.execute_raw(stmt(
        &db,
        format!(
            "INSERT INTO files_custom_metadata (file_id, key, value) VALUES ('{FILE}', 'tag', 'a')"
        ),
    ))
    .await
    .expect("insert metadata");

    db.execute_raw(stmt(
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

/// Rows inserted without the new columns backfill via column `DEFAULT` (`whole-sha256`, NULL
/// `part_count`); there is no data migration.
#[tokio::test]
async fn content_hash_modes_backfill_existing_rows_to_whole_sha256() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_version(&db, FILE, VERSION, 1).await;

    let row = db
        .query_one_raw(stmt(
            &db,
            format!(
                "SELECT hash_mode AS m, \
                 (SELECT COUNT(*) FROM file_versions WHERE part_count IS NULL) AS c \
                 FROM file_versions WHERE version_id = '{VERSION}'"
            ),
        ))
        .await
        .expect("query")
        .expect("one row");
    assert_eq!(
        row.try_get::<String>("", "m").expect("hash_mode"),
        "whole-sha256",
        "existing rows must backfill to whole-sha256"
    );
    assert_eq!(
        row.try_get::<i64>("", "c").expect("null part_count count"),
        1,
        "existing rows must have part_count NULL"
    );
    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT COUNT(*) AS c FROM version_hash_manifest WHERE version_id = '{VERSION}'"
            )
        )
        .await,
        0,
        "whole-sha256 versions must have no manifest row"
    );
}

#[tokio::test]
async fn content_hash_modes_rejects_multipart_without_part_count() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute_raw(stmt(
            &db,
            format!(
                "INSERT INTO file_versions \
                 (file_id, version_id, mime_type, size, hash_value, hash_mode, part_count, \
                  status, is_current, backend_id, backend_path) \
                 VALUES ('{FILE}', '{VERSION}', 'text/plain', 0, X'{HASH32}', \
                 'multipart-composite-sha256', NULL, 'available', 0, 'local', '/x')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "multipart-composite-sha256 with a NULL part_count must violate the presence CHECK"
    );
}

#[tokio::test]
async fn content_hash_modes_rejects_whole_with_part_count() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute_raw(stmt(
            &db,
            format!(
                "INSERT INTO file_versions \
                 (file_id, version_id, mime_type, size, hash_value, hash_mode, part_count, \
                  status, is_current, backend_id, backend_path) \
                 VALUES ('{FILE}', '{VERSION}', 'text/plain', 0, X'{HASH32}', \
                 'whole-sha256', 3, 'available', 0, 'local', '/x')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "whole-sha256 with a non-NULL part_count must violate the presence CHECK"
    );
}

#[tokio::test]
async fn content_hash_modes_rejects_unknown_hash_mode() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute_raw(stmt(
            &db,
            format!(
                "INSERT INTO file_versions \
                 (file_id, version_id, mime_type, size, hash_value, hash_mode, \
                  status, is_current, backend_id, backend_path) \
                 VALUES ('{FILE}', '{VERSION}', 'text/plain', 0, X'{HASH32}', \
                 'blake3-tree', 'available', 0, 'local', '/x')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "an unknown hash_mode must be rejected by the CHECK"
    );
}

#[tokio::test]
async fn content_hash_modes_leaves_hash_algorithm_check_intact() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute_raw(stmt(
            &db,
            format!(
                "INSERT INTO file_versions \
                 (file_id, version_id, mime_type, size, hash_algorithm, hash_value, \
                  status, is_current, backend_id, backend_path) \
                 VALUES ('{FILE}', '{VERSION}', 'text/plain', 0, 'BLAKE3', X'{HASH32}', \
                 'available', 0, 'local', '/x')"
            ),
        ))
        .await;
    assert!(
        res.is_err(),
        "hash_algorithm CHECK must still reject any non-SHA-256 value"
    );
}
