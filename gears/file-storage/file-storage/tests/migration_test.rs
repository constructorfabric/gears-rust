#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::MigratorTrait;
use toolkit_gts::gts_id;

use file_storage::Migrator;

const TENANT: &str = "00000000-0000-0000-0000-0000000000a1";
const OWNER: &str = "00000000-0000-0000-0000-0000000000b1";
const FILE: &str = "00000000-0000-0000-0000-0000000000c1";
const FILE2: &str = "00000000-0000-0000-0000-0000000000c2";
const VERSION: &str = "00000000-0000-0000-0000-0000000000d1";
const VERSION2: &str = "00000000-0000-0000-0000-0000000000d2";
const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");
const UPLOAD: &str = "00000000-0000-0000-0000-0000000000f2";
const HASH32: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn stmt(db: &DatabaseConnection, sql: impl Into<String>) -> Statement {
    Statement::from_string(db.get_database_backend(), sql.into())
}

async fn exec(db: &DatabaseConnection, sql: impl Into<String>) -> Result<(), DbErr> {
    db.execute_raw(stmt(db, sql)).await.map(drop)
}

/// Runs a statement the test expects to succeed.
async fn run(db: &DatabaseConnection, sql: impl Into<String>) {
    let sql = sql.into();
    exec(db, sql.as_str())
        .await
        .unwrap_or_else(|e| panic!("`{sql}` failed: {e}"));
}

/// Runs a statement a constraint must reject.
async fn rejected(db: &DatabaseConnection, sql: impl Into<String>) -> bool {
    exec(db, sql).await.is_err()
}

/// Fresh in-memory `SQLite`; foreign keys are enforced only when asked (`SQLite` defaults to off,
/// so cascades would silently no-op).
async fn fresh_db(foreign_keys: &str) -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    run(&db, format!("PRAGMA foreign_keys = {foreign_keys};")).await;
    db
}

/// Fresh database with every migration applied and foreign keys enforced.
async fn migrated_db() -> DatabaseConnection {
    let db = fresh_db("ON").await;
    Migrator::up(&db, None).await.expect("apply migrations");
    db
}

/// How many migrations are applied before the one whose name contains `name`.
fn migrations_before(name: &str) -> u32 {
    let names: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let index = names
        .iter()
        .position(|n| n.contains(name))
        .unwrap_or_else(|| panic!("no migration named `{name}` in {names:?}"));
    u32::try_from(index).expect("few migrations")
}

/// `down`/`up` of exactly one step touch `upload_flow` only while it is the last migration.
fn assert_upload_flow_is_last() {
    let total = u32::try_from(Migrator::migrations().len()).expect("few migrations");
    assert_eq!(migrations_before("upload_flow") + 1, total);
}

async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one_raw(stmt(db, sql))
        .await
        .expect("count query")
        .expect("one row")
        .try_get::<i64>("", "c")
        .expect("i64 column c")
}

/// `SELECT COUNT(*) FROM <table> WHERE <filter>`.
async fn rows_where(db: &DatabaseConnection, table: &str, filter: &str) -> i64 {
    count(
        db,
        &format!("SELECT COUNT(*) AS c FROM {table} WHERE {filter}"),
    )
    .await
}

async fn index_exists(db: &DatabaseConnection, name: &str) -> bool {
    rows_where(
        db,
        "sqlite_master",
        &format!("type = 'index' AND name = '{name}'"),
    )
    .await
        == 1
}

/// Whether `SELECT <cols> FROM <table>` plans, i.e. every column exists.
async fn columns_exist(db: &DatabaseConnection, table: &str, cols: &str) -> bool {
    exec(db, format!("SELECT {cols} FROM {table} LIMIT 0"))
        .await
        .is_ok()
}

async fn insert_file(db: &DatabaseConnection, file_id: &str) {
    run(
        db,
        format!(
            "INSERT INTO files (file_id, tenant_id, owner_kind, owner_id, name, gts_file_type) \
             VALUES ('{file_id}', '{TENANT}', 'user', '{OWNER}', 'doc.txt', '{GTS}')"
        ),
    )
    .await;
}

fn version_insert(file_id: &str, version_id: &str, is_current: u8) -> String {
    format!(
        "INSERT INTO file_versions \
         (file_id, version_id, mime_type, size, hash_value, status, is_current, backend_id, backend_path) \
         VALUES ('{file_id}', '{version_id}', 'text/plain', 0, X'{HASH32}', 'available', {is_current}, 'local', '/{file_id}/{version_id}')"
    )
}

async fn insert_version(db: &DatabaseConnection, file_id: &str, version_id: &str, is_current: u8) {
    run(db, version_insert(file_id, version_id, is_current)).await;
}

/// A `multipart_uploads` row in `state` (`declared_size` bytes in `part_size` parts).
async fn insert_session(db: &DatabaseConnection, state: &str, declared_size: u32, part_size: u32) {
    run(
        db,
        format!(
            "INSERT INTO multipart_uploads \
             (upload_id, file_id, version_id, backend_upload_handle, state, declared_mime, \
              mime_validated, declared_size, part_size, expires_at) \
             VALUES ('{UPLOAD}', '{FILE}', '{VERSION}', 'handle-1', '{state}', 'text/plain', \
                     1, {declared_size}, {part_size}, '2999-01-01T00:00:00Z')"
        ),
    )
    .await;
}

async fn insert_part(db: &DatabaseConnection, part_number: u8, size: u32) {
    run(
        db,
        format!(
            "INSERT INTO multipart_upload_parts \
             (upload_id, part_number, backend_etag, part_hash, size) \
             VALUES ('{UPLOAD}', {part_number}, 'etag-{part_number}', X'{HASH32}', {size})"
        ),
    )
    .await;
}

/// `policies` ids are `TEXT` in the `SQLite` DDL, so UUIDs are plain quoted literals.
fn policy_insert(
    policy_id: &str,
    tenant: &str,
    scope: &str,
    scope_owner: Option<&str>,
    body: &str,
    updated_at: &str,
) -> String {
    let scope_owner = scope_owner.map_or_else(|| "NULL".to_owned(), |o| format!("'{o}'"));
    format!(
        "INSERT INTO policies (policy_id, tenant_id, scope, scope_owner_id, body, updated_at) \
         VALUES ('{policy_id}', '{tenant}', '{scope}', {scope_owner}, '{body}', '{updated_at}')"
    )
}

#[tokio::test]
async fn migration_up_down_up_roundtrip() {
    let db = migrated_db().await;
    for table in ["files", "file_versions", "files_custom_metadata"] {
        assert!(
            columns_exist(&db, table, "*").await,
            "table {table} must exist after up"
        );
    }

    Migrator::down(&db, None).await.expect("roll back");
    assert!(
        !columns_exist(&db, "files", "*").await,
        "files must be dropped by down()"
    );

    Migrator::up(&db, None).await.expect("re-apply");
    assert!(
        columns_exist(&db, "files", "*").await,
        "files must exist again after re-up()"
    );
}

const UPLOAD_FLOW_MULTIPART_COLS: [&str; 6] = [
    "auto_bind",
    "lease_until",
    "lease_owner",
    "complete_result",
    "backend_id",
    "backend_path",
];

/// `down()` of the last migration (`upload_flow`) drops its column, index and `multipart_uploads`
/// columns while keeping the pre-existing ones, and re-`up()` restores them.
#[tokio::test]
async fn upload_flow_down_up_drops_and_restores_columns_and_indexes() {
    assert_upload_flow_is_last();
    let db = migrated_db().await;
    let mp_cols = UPLOAD_FLOW_MULTIPART_COLS.join(", ");
    let applied = || async {
        (
            columns_exist(&db, "file_versions", "bound_on_finalize").await,
            index_exists(&db, "idempotency_keys_file_idx").await,
            columns_exist(&db, "multipart_uploads", &mp_cols).await,
        )
    };
    assert_eq!(applied().await, (true, true, true), "after the full up()");

    Migrator::down(&db, Some(1))
        .await
        .expect("roll back upload_flow");
    assert_eq!(applied().await, (false, false, false), "after down()");
    for col in UPLOAD_FLOW_MULTIPART_COLS {
        assert!(!columns_exist(&db, "multipart_uploads", col).await, "{col}");
    }
    assert!(
        columns_exist(
            &db,
            "multipart_uploads",
            "declared_size, part_size, mime_validated, created_at, expires_at"
        )
        .await,
        "pre-existing columns must remain"
    );
    for index in [
        "multipart_uploads_file_idx",
        "multipart_uploads_expired_idx",
        "files_owner_listing_v2_idx",
    ] {
        assert!(
            index_exists(&db, index).await,
            "{index} must survive down()"
        );
    }

    Migrator::up(&db, Some(1))
        .await
        .expect("re-apply upload_flow");
    assert_eq!(applied().await, (true, true, true), "after re-up()");
}

/// `down()` folds a `completing` session into `aborted` (keeping its parts and the pre-existing
/// column values) and narrows the state CHECK so `completing` is rejected again.
#[tokio::test]
async fn upload_flow_down_folds_completing_session_into_aborted() {
    assert_upload_flow_is_last();
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_session(&db, "completing", 5, 5).await;
    insert_part(&db, 1, 5).await;

    Migrator::down(&db, Some(1))
        .await
        .expect("roll back upload_flow");

    assert_eq!(
        rows_where(
            &db,
            "multipart_uploads",
            &format!("upload_id = '{UPLOAD}' AND state = 'aborted' AND declared_size = 5 AND part_size = 5"),
        )
        .await,
        1,
        "a completing session must be folded into aborted"
    );
    assert_eq!(
        rows_where(&db, "multipart_upload_parts", "1 = 1").await,
        1,
        "parts must survive the rebuild"
    );
    assert!(
        rejected(
            &db,
            format!(
                "UPDATE multipart_uploads SET state = 'completing' WHERE upload_id = '{UPLOAD}'"
            )
        )
        .await,
        "the narrowed CHECK must reject completing"
    );
}

/// `up()` over the old schema: an existing version backfills `bound_on_finalize = false`, and the
/// `multipart_uploads` rebuild keeps sessions, parts, indexes and cascades, widening the state
/// CHECK to admit `completing`.
#[tokio::test]
async fn upload_flow_up_backfills_and_preserves_existing_rows() {
    let db = fresh_db("ON").await;
    Migrator::up(&db, Some(migrations_before("upload_flow")))
        .await
        .expect("apply every migration before upload_flow");

    insert_file(&db, FILE).await;
    insert_version(&db, FILE, VERSION, 1).await;
    insert_session(&db, "completed", 300, 100).await;
    insert_part(&db, 1, 100).await;
    insert_part(&db, 2, 200).await;

    Migrator::up(&db, None).await.expect("apply upload_flow");

    assert_eq!(
        rows_where(
            &db,
            "file_versions",
            &format!("version_id = '{VERSION}' AND bound_on_finalize = 0")
        )
        .await,
        1,
        "a version that existed before this migration must default bound_on_finalize to false"
    );
    assert_eq!(
        rows_where(
            &db,
            "multipart_upload_parts",
            &format!("upload_id = '{UPLOAD}'")
        )
        .await,
        2,
        "both parts must survive the multipart_uploads rebuild"
    );
    assert_eq!(
        rows_where(
            &db,
            "multipart_uploads",
            &format!(
                "upload_id = '{UPLOAD}' AND state = 'completed' AND mime_validated = 1 \
                 AND declared_size = 300 AND part_size = 100 AND declared_mime = 'text/plain' \
                 AND backend_upload_handle = 'handle-1'"
            ),
        )
        .await,
        1,
        "the session row, with every pre-existing column value, must survive the rebuild"
    );
    for index in [
        "multipart_uploads_file_idx",
        "multipart_uploads_expired_idx",
    ] {
        assert!(
            index_exists(&db, index).await,
            "{index} must be recreated by the rebuild"
        );
    }

    // The widened state CHECK admits `completing`; an unknown state is still rejected.
    run(
        &db,
        format!("UPDATE multipart_uploads SET state = 'completing' WHERE upload_id = '{UPLOAD}'"),
    )
    .await;
    assert!(
        rejected(
            &db,
            format!("UPDATE multipart_uploads SET state = 'bogus' WHERE upload_id = '{UPLOAD}'")
        )
        .await,
        "an unknown state must violate the CHECK"
    );

    // The rebuilt FK still cascades: deleting the file removes its sessions and their parts.
    run(&db, format!("DELETE FROM files WHERE file_id = '{FILE}'")).await;
    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM multipart_uploads").await,
        0
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM multipart_upload_parts").await,
        0,
        "parts must cascade with their session"
    );
}

/// Every CHECK constraint on `files` / `file_versions` rejects a violating row; the base file row
/// exists so a failure is attributable to the CHECK, not the foreign key.
#[tokio::test]
async fn check_constraints_reject_invalid_rows() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let version_columns =
        "file_id, version_id, mime_type, size, hash_value, backend_id, backend_path";
    let version_values =
        format!("'{FILE}', '{VERSION}', 'text/plain', 0, X'{HASH32}', 'local', '/p'");
    let cases: Vec<(&str, String)> = vec![
        (
            "files.owner_kind 'robot'",
            format!(
                "INSERT INTO files (file_id, tenant_id, owner_kind, owner_id, name, gts_file_type) \
                 VALUES ('{FILE2}', '{TENANT}', 'robot', '{OWNER}', 'a', '{GTS}')"
            ),
        ),
        (
            "files.meta_version -1",
            format!(
                "INSERT INTO files (file_id, tenant_id, owner_kind, owner_id, name, gts_file_type, meta_version) \
                 VALUES ('{FILE2}', '{TENANT}', 'user', '{OWNER}', 'a', '{GTS}', -1)"
            ),
        ),
        (
            "file_versions.size -1",
            format!(
                "INSERT INTO file_versions (file_id, version_id, mime_type, size, hash_value, backend_id, backend_path) \
                 VALUES ('{FILE}', '{VERSION}', 'text/plain', -1, X'{HASH32}', 'local', '/p')"
            ),
        ),
        (
            "file_versions.status 'frozen'",
            format!(
                "INSERT INTO file_versions (file_id, version_id, mime_type, size, hash_value, status, backend_id, backend_path) \
                 VALUES ('{FILE}', '{VERSION}', 'text/plain', 0, X'{HASH32}', 'frozen', 'local', '/p')"
            ),
        ),
        (
            "file_versions.hash_algorithm 'BLAKE3' (only SHA-256 is allowed)",
            format!(
                "INSERT INTO file_versions (file_id, version_id, mime_type, size, hash_algorithm, hash_value, backend_id, backend_path) \
                 VALUES ('{FILE}', '{VERSION}', 'text/plain', 0, 'BLAKE3', X'{HASH32}', 'local', '/p')"
            ),
        ),
        (
            "file_versions.hash_value of 4 bytes",
            format!(
                "INSERT INTO file_versions (file_id, version_id, mime_type, size, hash_value, backend_id, backend_path) \
                 VALUES ('{FILE}', '{VERSION}', 'text/plain', 0, X'00112233', 'local', '/p')"
            ),
        ),
        (
            "multipart-composite-sha256 with NULL part_count",
            format!(
                "INSERT INTO file_versions ({version_columns}, hash_mode, part_count) \
                 VALUES ({version_values}, 'multipart-composite-sha256', NULL)"
            ),
        ),
        (
            "whole-sha256 with a part_count",
            format!(
                "INSERT INTO file_versions ({version_columns}, hash_mode, part_count) \
                 VALUES ({version_values}, 'whole-sha256', 3)"
            ),
        ),
        (
            "unknown hash_mode",
            format!(
                "INSERT INTO file_versions ({version_columns}, hash_mode) \
                 VALUES ({version_values}, 'blake3-tree')"
            ),
        ),
    ];
    for (name, sql) in cases {
        assert!(rejected(&db, sql).await, "CHECK must reject {name}");
    }
    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM file_versions").await,
        0,
        "no rejected row may be stored"
    );
}

/// The partial unique index on `is_current`: one current version per file, any number of
/// non-current ones, and one current version for each of two files.
#[tokio::test]
async fn file_versions_current_is_unique_per_file() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_file(&db, FILE2).await;
    insert_version(&db, FILE, VERSION, 1).await;
    assert!(
        rejected(&db, version_insert(FILE, VERSION2, 1)).await,
        "two current versions of one file must violate the unique index"
    );
    insert_version(&db, FILE, VERSION2, 0).await;
    insert_version(&db, FILE, "00000000-0000-0000-0000-0000000000d3", 0).await;
    insert_version(&db, FILE2, "00000000-0000-0000-0000-0000000000d4", 1).await;
    assert_eq!(
        rows_where(&db, "file_versions", "is_current = 1").await,
        2,
        "one current version per file"
    );
}

#[tokio::test]
async fn custom_metadata_rejects_duplicate_key_per_file() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    let insert = |value: &str| {
        format!(
            "INSERT INTO files_custom_metadata (file_id, key, value) VALUES ('{FILE}', 'tag', '{value}')"
        )
    };
    run(&db, insert("a")).await;
    assert!(
        rejected(&db, insert("b")).await,
        "(file_id, key) PK must reject duplicate key"
    );
}

/// `request_hash` is `NOT NULL DEFAULT` empty blob: an INSERT omitting it succeeds, and such a row
/// fails closed on any replay.
#[tokio::test]
async fn idempotency_keys_request_hash_defaults_to_empty_blob() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    run(
        &db,
        format!(
            "INSERT INTO idempotency_keys \
             (tenant_id, owner_kind, owner_id, idempotency_key, file_id, \
              response_status, response_body, response_etag, expires_at) \
             VALUES ('{TENANT}', 'user', '{OWNER}', 'k1', '{FILE}', \
                     201, '{{}}', 'etag', '2999-01-01T00:00:00Z')"
        ),
    )
    .await;
    let hash_len = db
        .query_one_raw(stmt(
            &db,
            "SELECT LENGTH(request_hash) AS c FROM idempotency_keys WHERE idempotency_key = 'k1'",
        ))
        .await
        .expect("select request_hash length")
        .expect("one row")
        .try_get::<i64>("", "c")
        .expect("i64 column c");
    assert_eq!(hash_len, 0, "request_hash must default to an empty blob");
}

/// A plain `UNIQUE` would not catch a duplicate tenant-scope row (`NULL`s are distinct), hence the
/// partial indexes; distinct scopes, owners and tenants must not collide.
#[tokio::test]
async fn policies_unique_indexes_reject_duplicates_and_allow_distinct_scopes() {
    let db = migrated_db().await;
    let tenant2 = "00000000-0000-0000-0000-0000000000a2";
    let owner2 = "00000000-0000-0000-0000-0000000000b2";
    let insert = |id: &str, tenant: &str, scope: &str, owner: Option<&str>| {
        policy_insert(id, tenant, scope, owner, "{}", "2026-01-01T00:00:00Z")
    };
    let id = |n: u8| format!("00000000-0000-0000-0000-0000000000e{n}");

    run(&db, insert(&id(1), TENANT, "user", Some(owner2))).await;
    assert!(
        rejected(&db, insert(&id(2), TENANT, "user", Some(owner2))).await,
        "duplicate (tenant_id, 'user', scope_owner_id) must violate the user-scope index"
    );
    run(&db, insert(&id(3), TENANT, "tenant", None)).await;
    assert!(
        rejected(&db, insert(&id(4), TENANT, "tenant", None)).await,
        "duplicate (tenant_id, 'tenant') with NULL scope_owner_id must violate the tenant-scope index"
    );

    run(&db, insert(&id(5), TENANT, "user", Some(OWNER))).await;
    run(&db, insert(&id(6), tenant2, "user", Some(owner2))).await;
    run(&db, insert(&id(7), tenant2, "tenant", None)).await;
    assert_eq!(count(&db, "SELECT COUNT(*) AS c FROM policies").await, 5);
}

/// Applies all but the `policies_unique_scope` migration and seeds duplicates (as the old upsert
/// race could leave), then applies it: it must dedup to the newest row before creating the indexes.
#[tokio::test]
async fn policies_unique_migration_dedups_preexisting_duplicates() {
    let db = fresh_db("OFF").await;
    Migrator::up(&db, Some(migrations_before("policies_unique_scope")))
        .await
        .expect("apply migrations before policies_unique_scope");

    let owner = "00000000-0000-0000-0000-0000000000b2";
    let id = |n: u8| format!("00000000-0000-0000-0000-0000000000e{n}");
    // (id, scope, owner, updated_at): per scope an older and a newer row.
    for (n, scope, scope_owner, updated_at) in [
        (1, "user", Some(owner), "2026-01-01T00:00:00Z"),
        (2, "user", Some(owner), "2026-06-01T00:00:00Z"),
        (3, "tenant", None, "2026-01-01T00:00:00Z"),
        (4, "tenant", None, "2026-06-01T00:00:00Z"),
    ] {
        run(
            &db,
            policy_insert(&id(n), TENANT, scope, scope_owner, "{\"v\":1}", updated_at),
        )
        .await;
    }
    assert_eq!(count(&db, "SELECT COUNT(*) AS c FROM policies").await, 4);

    Migrator::up(&db, Some(1))
        .await
        .expect("policies_unique_scope must dedup before creating the unique indexes");

    for (scope, filter, newer, older) in [
        (
            "user",
            format!("scope = 'user' AND scope_owner_id = '{owner}'"),
            id(2),
            id(1),
        ),
        (
            "tenant",
            "scope = 'tenant' AND scope_owner_id IS NULL".to_owned(),
            id(4),
            id(3),
        ),
    ] {
        assert_eq!(
            rows_where(&db, "policies", &filter).await,
            1,
            "{scope}: duplicates must be deduped to exactly one"
        );
        assert_eq!(
            rows_where(&db, "policies", &format!("policy_id = '{newer}'")).await,
            1,
            "{scope}: the survivor must be the most recently updated row"
        );
        assert_eq!(
            rows_where(&db, "policies", &format!("policy_id = '{older}'")).await,
            0,
            "{scope}: the stale duplicate must be deleted"
        );
    }
}

#[tokio::test]
async fn deleting_file_cascades_to_versions_and_metadata() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_version(&db, FILE, VERSION, 1).await;
    run(
        &db,
        format!(
            "INSERT INTO files_custom_metadata (file_id, key, value) VALUES ('{FILE}', 'tag', 'a')"
        ),
    )
    .await;

    run(&db, format!("DELETE FROM files WHERE file_id = '{FILE}'")).await;

    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM file_versions").await,
        0
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM files_custom_metadata").await,
        0
    );
}

/// A version row inserted without the hash-mode columns gets the column `DEFAULT`s:
/// `whole-sha256` and a NULL `part_count`.
#[tokio::test]
async fn file_versions_default_to_whole_sha256_without_part_count() {
    let db = migrated_db().await;
    insert_file(&db, FILE).await;
    insert_version(&db, FILE, VERSION, 1).await;
    assert_eq!(
        rows_where(
            &db,
            "file_versions",
            &format!(
                "version_id = '{VERSION}' AND hash_mode = 'whole-sha256' AND part_count IS NULL"
            )
        )
        .await,
        1
    );
}

/// Shared body of the `upload_flow` data-preservation tests: applies everything before it, seeds a
/// session with a version row, a session without one and two parts, then applies the rest.
async fn upload_flow_backfills_sessions_and_keeps_parts(foreign_keys: &str) {
    let db = fresh_db(foreign_keys).await;
    Migrator::up(&db, Some(migrations_before("upload_flow")))
        .await
        .expect("apply migrations before upload_flow");

    let upload_with_version = "00000000-0000-0000-0000-0000000000e1";
    let upload_without_version = "00000000-0000-0000-0000-0000000000e2";

    insert_file(&db, FILE).await;
    insert_version(&db, FILE, VERSION, 0).await;
    insert_file(&db, FILE2).await;

    for (upload_id, file_id, version_id) in [
        (upload_with_version, FILE, VERSION),
        (upload_without_version, FILE2, VERSION2),
    ] {
        run(
            &db,
            format!(
                "INSERT INTO multipart_uploads \
                 (upload_id, file_id, version_id, backend_upload_handle, state, declared_mime, \
                  mime_validated, declared_size, part_size, created_at, expires_at) \
                 VALUES ('{upload_id}', '{file_id}', '{version_id}', 'h', 'in_progress', \
                         'text/plain', 0, 10, 5, '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z')"
            ),
        )
        .await;
    }
    for part in 1..=2 {
        run(
            &db,
            format!(
                "INSERT INTO multipart_upload_parts \
                 (upload_id, part_number, backend_etag, part_hash, size) \
                 VALUES ('{upload_with_version}', {part}, 'etag{part}', X'{HASH32}', 5)"
            ),
        )
        .await;
    }

    Migrator::up(&db, None)
        .await
        .expect("upload_flow migration must pass regardless of the foreign_keys pragma");

    let backend_pair = |upload_id: &str| {
        let db = &db;
        let sql = format!(
            "SELECT backend_id, backend_path FROM multipart_uploads WHERE upload_id = '{upload_id}'"
        );
        async move {
            let row = db
                .query_one_raw(stmt(db, sql))
                .await
                .expect("query session")
                .expect("session row survives");
            (
                row.try_get::<Option<String>>("", "backend_id").unwrap(),
                row.try_get::<Option<String>>("", "backend_path").unwrap(),
            )
        }
    };
    assert_eq!(
        backend_pair(upload_with_version).await,
        (Some("local".to_owned()), Some(format!("/{FILE}/{VERSION}"))),
        "a session whose version exists backfills the version's backend pair"
    );
    assert_eq!(
        backend_pair(upload_without_version).await,
        (None, None),
        "a session without a version row stays NULL"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) AS c FROM multipart_upload_parts").await,
        2,
        "both parts must survive the rebuild"
    );
}

#[tokio::test]
async fn upload_flow_backfills_backend_pair_and_keeps_parts_with_fk_on() {
    upload_flow_backfills_sessions_and_keeps_parts("ON").await;
}

#[tokio::test]
async fn upload_flow_migration_passes_with_fk_off() {
    upload_flow_backfills_sessions_and_keeps_parts("OFF").await;
}
