//! Schema-level tests for the P1 initial migration, run against a real
//! in-memory SQLite database (~1ms per DB). These verify that the SQL itself is
//! correct — every `CHECK` constraint, the partial unique "current version"
//! index, composite primary keys, and `ON DELETE CASCADE` — without needing a
//! running server. PostgreSQL-dialect behaviour (domain types, schema
//! namespace, FK RESTRICT) is covered by E2E tests, not here.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use toolkit_gts::gts_id;

use file_storage::Migrator;

const TENANT: &str = "00000000-0000-0000-0000-0000000000a1";
const OWNER: &str = "00000000-0000-0000-0000-0000000000b1";
const FILE: &str = "00000000-0000-0000-0000-0000000000c1";
/// Version id used by the ADR-0006 content-hash-modes tests appended at the
/// end of this file.
const VERSION: &str = "00000000-0000-0000-0000-0000000000d1";
const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");
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

// ── idempotency_keys additive columns (P2 remediation) ───────────────────────

/// P2 remediation 2.1: `request_hash` binds a replay to the request body that
/// created it. The column must be additive-safe — an INSERT that omits it
/// (as every pre-2.1 write path effectively did) must succeed and default to
/// an empty blob, never a constraint violation, and never NULL (a NULL would
/// compare unequal to itself in a naive check, and more importantly could
/// never legitimately match a freshly computed 32-byte SHA-256 either way —
/// but `NOT NULL DEFAULT` is the deliberate choice so a pre-migration/omitted
/// row fails closed on any future replay rather than silently passing).
#[tokio::test]
async fn idempotency_keys_request_hash_column_exists_with_default() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    db.execute(stmt(
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
        .query_one(stmt(
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

// ── policies partial unique indexes (P2 remediation 2.4) ─────────────────────

/// P2 remediation 2.4: `policies_user_scope_unique_idx` enforces at most one
/// row per `(tenant_id, 'user', scope_owner_id)`. Two concurrent `PUT
/// /policy` calls for the same user scope used to be able to leave two rows
/// (delete-then-insert with no transaction and no unique constraint); this
/// index turns the second writer's insert into a hard constraint violation
/// instead. `policies.tenant_id` / `scope_owner_id` are declared `TEXT` in
/// this gear's SQLite DDL (see `m20260701_000001_p2_initial.rs`), not
/// `BLOB`, so plain quoted UUID string literals are the correct raw-SQL
/// representation here (unlike `hash_value`/`request_hash`, which are
/// declared `BLOB` and need `X'...'` literals).
#[tokio::test]
async fn policies_unique_index_rejects_duplicate_scope_tuple() {
    let db = migrated_db().await;
    let owner2 = "00000000-0000-0000-0000-0000000000b2";
    db.execute(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body) \
             VALUES ('00000000-0000-0000-0000-0000000000e1', '{TENANT}', 'user', '{owner2}', '{{}}')"
        ),
    ))
    .await
    .expect("first user-scope policy insert");

    let res = db
        .execute(stmt(
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

/// P2 remediation 2.4: `policies_tenant_scope_unique_idx` enforces at most
/// one row per `(tenant_id, 'tenant')` (i.e. `scope_owner_id IS NULL`). A
/// plain `UNIQUE (tenant_id, scope, scope_owner_id)` index would NOT catch
/// this — Postgres/SQLite both treat every `NULL` as distinct — hence the
/// dedicated partial index scoped to `scope_owner_id IS NULL`.
#[tokio::test]
async fn policies_unique_index_rejects_duplicate_tenant_scope() {
    let db = migrated_db().await;
    db.execute(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body) \
             VALUES ('00000000-0000-0000-0000-0000000000e3', '{TENANT}', 'tenant', NULL, '{{}}')"
        ),
    ))
    .await
    .expect("first tenant-scope policy insert");

    let res = db
        .execute(stmt(
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

/// Sanity check that the partial indexes don't over-constrain: two different
/// tenants can each have their own user-scope row for the same owner id, and
/// a tenant-scope row coexists fine with user-scope rows for the same
/// tenant.
#[tokio::test]
async fn policies_unique_index_allows_distinct_scopes() {
    let db = migrated_db().await;
    let tenant2 = "00000000-0000-0000-0000-0000000000a2";
    db.execute(stmt(
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

/// P2 remediation 2.4 follow-up (CodeRabbit finding on PR #4184): the two
/// partial unique indexes are created with a plain `CREATE UNIQUE INDEX`,
/// which fails outright if the table already contains rows that would
/// violate the new constraint. That is exactly the state the pre-2.4 upsert
/// race (`DELETE` then independent `INSERT`, no transaction, no unique
/// constraint) could have left behind, so the migration must dedup existing
/// duplicate rows before creating either index, or it can never be applied
/// to a database that hit the race even once.
///
/// This test applies every migration up to (but not including)
/// `m20260706_000003_policies_unique_scope` — i.e. the first five migrations
/// registered in `Migrator::migrations()` — inserts two duplicate
/// user-scope `policies` rows directly (bypassing the not-yet-created
/// unique index), then applies the sixth migration and asserts it succeeds,
/// dedups down to the most-recently-updated row, and leaves the index
/// enforcing uniqueness going forward.
#[tokio::test]
async fn policies_unique_migration_dedups_preexisting_duplicates() {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");

    // Apply every migration except the last one (policies_unique_scope), so
    // the `policies` table exists but neither partial unique index does yet.
    Migrator::up(&db, Some(5))
        .await
        .expect("apply migrations up to (not including) policies_unique_scope");

    let owner = "00000000-0000-0000-0000-0000000000b2";
    let older = "00000000-0000-0000-0000-0000000000e1";
    let newer = "00000000-0000-0000-0000-0000000000e2";

    // Two duplicate rows for the same (tenant_id, 'user', scope_owner_id)
    // tuple -- exactly what the pre-2.4 upsert race could produce. `newer`
    // has a later `updated_at` and must be the row that survives dedup.
    db.execute(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body, updated_at) \
             VALUES ('{older}', '{TENANT}', 'user', '{owner}', '{{\"v\":1}}', '2026-01-01T00:00:00Z')"
        ),
    ))
    .await
    .expect("insert older duplicate user-scope policy");
    db.execute(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body, updated_at) \
             VALUES ('{newer}', '{TENANT}', 'user', '{owner}', '{{\"v\":2}}', '2026-06-01T00:00:00Z')"
        ),
    ))
    .await
    .expect("insert newer duplicate user-scope policy");

    // Also seed a tenant-scope duplicate pair (scope_owner_id IS NULL) to
    // exercise the second dedup pass / second partial index.
    let tenant_older = "00000000-0000-0000-0000-0000000000e3";
    let tenant_newer = "00000000-0000-0000-0000-0000000000e4";
    db.execute(stmt(
        &db,
        format!(
            "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body, updated_at) \
             VALUES ('{tenant_older}', '{TENANT}', 'tenant', NULL, '{{\"v\":1}}', '2026-01-01T00:00:00Z')"
        ),
    ))
    .await
    .expect("insert older duplicate tenant-scope policy");
    db.execute(stmt(
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

    // Apply the remaining migration (policies_unique_scope). This must not
    // fail even though duplicates exist.
    Migrator::up(&db, Some(1))
        .await
        .expect("policies_unique_scope migration must dedup before creating the unique indexes");

    // Exactly one row per group must survive, and it must be the
    // most-recently-updated one.
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

    // The partial unique indexes must now be live: a fresh duplicate insert
    // is rejected.
    let dup_res = db
        .execute(stmt(
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

// ── ADR-0006 content-hash modes: hash_mode / part_count / manifest ───────────

/// AC6: a pre-existing `file_versions` row (inserted without the ADR-0006
/// columns, exactly as P1/P2 code did) backfills to `hash_mode =
/// 'whole-sha256'`, `part_count = NULL`, and has no `version_hash_manifest`
/// row — driven entirely by the column DEFAULT, with no data migration.
#[tokio::test]
async fn content_hash_modes_backfill_existing_rows_to_whole_sha256() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    // `insert_version` deliberately does NOT mention hash_mode/part_count.
    insert_version(&db, FILE, VERSION, 1).await;

    let row = db
        .query_one(stmt(
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

/// The cross-column presence CHECK rejects `part_count IS NULL` for a
/// `multipart-composite-sha256` row.
#[tokio::test]
async fn content_hash_modes_rejects_multipart_without_part_count() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute(stmt(
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

/// The cross-column presence CHECK rejects a non-NULL `part_count` for a
/// `whole-sha256` row.
#[tokio::test]
async fn content_hash_modes_rejects_whole_with_part_count() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute(stmt(
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

/// The `hash_mode` CHECK rejects any value outside the two shipped modes.
#[tokio::test]
async fn content_hash_modes_rejects_unknown_hash_mode() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute(stmt(
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

/// The `hash_algorithm = 'SHA-256'` CHECK is left untouched by ADR-0006 — a
/// second algorithm is still rejected (AC5, schema half).
#[tokio::test]
async fn content_hash_modes_leaves_hash_algorithm_check_intact() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let res = db
        .execute(stmt(
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
